//! Native batch page. Workers own their data and report through channels, never HWND pointers.
use super::*;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use crate::batch_layout::StatusContent;
use crate::batch_queue::Queue;
use crate::test_status_icons::{self, StatusIcon};
use dictate_core::batch_transcription::{
    AUDIO_FILE_EXTENSIONS, BatchControl, BatchSummary, BatchUpdate, CollectedFiles, FileStatus,
    collect_files,
};
use dictate_core::config::BatchTranscriptionConfig;
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{GetDC, GetTextExtentPoint32W, ReleaseDC};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Controls::{
    ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TOOLTIPS_CLASSW, TTF_IDISHWND,
    TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETMAXTIPWIDTH, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP,
    TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_DELETE;
use windows::Win32::UI::Shell::{
    Common::COMDLG_FILTERSPEC, DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW,
    FOLDERID_Documents, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, HDROP, IFileOpenDialog,
    KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath, SIGDN_FILESYSPATH, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PWSTR;

pub const GROUP: &str = "Batch";
pub const TIMER: usize = 0x6d00;
pub const ID_LIST: usize = 0x6d01;
const ANIMATION_TIMER: usize = 0x6d08;
const ID_OUTPUT: usize = 0x6d02;
const ID_SIZE: usize = 0x6d03;
const ID_FILES: usize = 0x6d04;
const ID_FOLDERS: usize = 0x6d05;
const ID_TRANSCRIBE: usize = 0x6d06;
const ID_RETRY: usize = 0x6d07;
const LIST_TOP: i32 = 120;
const LIST_HEIGHT: i32 = 300;
const LIST_OPTIONS: i32 =
    LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | LBS_EXTENDEDSEL;

enum WorkerEvent {
    Update(BatchUpdate),
    Finished(Result<BatchSummary, String>),
}

#[derive(Default)]
pub struct Page {
    pub queue: Queue,
    pub activity: std::rc::Rc<std::cell::Cell<bool>>,
    list: HWND,
    scrollbar: HWND,
    receiver: Option<Receiver<WorkerEvent>>,
    cancellation: tokio_util::sync::CancellationToken,
    control: BatchControl,
    imports: Vec<Receiver<CollectedFiles>>,
    import_cancellation: tokio_util::sync::CancellationToken,
    additions: VecDeque<PathBuf>,
    close_requested: bool,
    animation_started: Option<Instant>,
    animation_timer_running: bool,
    tooltip: HWND,
    tooltip_text: Vec<u16>,
}

impl Drop for Page {
    fn drop(&mut self) {
        self.activity.set(false);
        self.cancellation.cancel();
        self.import_cancellation.cancel();
    }
}

pub fn create(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let config = state.runtime.config().batch_transcription;
    let output = if config.output_dir.trim().is_empty() {
        // Resolving a default must never leave the settings window half-created.
        // An unavailable path stays editable and is checked when a batch starts.
        default_output_directory()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        config.output_dir
    };
    for (key, y) in [
        ("batch_files", 90),
        ("batch_output_directory", 432),
        ("batch_size", 504),
    ] {
        let label = create_label(
            state,
            state.language.text(key),
            CONTENT_LEFT,
            y,
            450,
            28,
            instance,
        )?;
        state.control_groups.insert(label.0 as usize, GROUP);
        state.localized_controls.push((label, key));
    }
    let list = create_child(
        state,
        w!("LISTBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(LIST_OPTIONS as u32),
        CONTENT_LEFT + 6,
        LIST_TOP + 6,
        510,
        LIST_HEIGHT - 12,
        ID_LIST,
        instance,
    )?;
    state.batch.list = list;
    state.controls.insert("__batch_list", list);
    state.control_groups.insert(list.0 as usize, GROUP);
    state.input_frames.push(InputFrame {
        rect: RECT {
            left: CONTENT_LEFT,
            top: LIST_TOP,
            right: EDIT_LEFT + EDIT_WIDTH,
            bottom: LIST_TOP + LIST_HEIGHT,
        },
        group: GROUP,
        control: list,
    });
    apply_dark_theme(list);
    dropdown::track_hover(list, true)?;
    unsafe {
        DragAcceptFiles(list, true);
        SetWindowSubclass(list, Some(list_proc), 25, state.hwnd.0 as usize)
            .ok()
            .map_err(|e| e.to_string())?;
    }
    let scrollbar = dropdown_scrollbar::create(state, state.hwnd, list, instance)?;
    dropdown_scrollbar::set_background(scrollbar, rgb(26, 35, 39));
    state.batch.scrollbar = scrollbar;
    state.controls.insert("__batch_scrollbar", scrollbar);
    state.control_groups.insert(scrollbar.0 as usize, GROUP);
    for (key, id, y, text) in [
        ("__batch_output", ID_OUTPUT, 462, output),
        ("__batch_size", ID_SIZE, 534, config.batch_size.to_string()),
    ] {
        let edit = create_child(
            state,
            w!("EDIT"),
            &text,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            CONTENT_LEFT + 10,
            y + 6,
            EDIT_LEFT + EDIT_WIDTH - CONTENT_LEFT - 20,
            22,
            id,
            instance,
        )?;
        state.controls.insert(key, edit);
        state.control_groups.insert(edit.0 as usize, GROUP);
        state.input_frames.push(InputFrame {
            rect: RECT {
                left: CONTENT_LEFT,
                top: y,
                right: EDIT_LEFT + EDIT_WIDTH,
                bottom: y + 34,
            },
            group: GROUP,
            control: edit,
        });
        apply_dark_theme(edit);
    }
    for (index, (key, id)) in [
        ("batch_files", ID_FILES),
        ("batch_folders", ID_FOLDERS),
        ("batch_transcribe", ID_TRANSCRIBE),
        ("batch_retry", ID_RETRY),
    ]
    .into_iter()
    .enumerate()
    {
        let button = create_child(
            state,
            w!("BUTTON"),
            state.language.text(key),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            CONTENT_LEFT + index as i32 * 138,
            588,
            122,
            34,
            id,
            instance,
        )?;
        state.controls.insert(key, button);
        state.control_groups.insert(button.0 as usize, GROUP);
        state.localized_controls.push((button, key));
        dropdown::track_hover(button, false)?;
    }
    create_tooltip(state, instance)?;
    resize(state);
    unsafe {
        SetTimer(Some(state.hwnd), TIMER, 50, None);
    }
    Ok(())
}

pub fn resize(state: &SettingsState) {
    if state.batch.list.is_invalid() {
        return;
    }
    let s = |value| platform::scale(value, state.dpi);
    let right = s(EDIT_LEFT + EDIT_WIDTH - 6);
    let height = s(LIST_HEIGHT - 12);
    let row_height = s(36).max(1);
    let count = unsafe { SendMessageW(state.batch.list, LB_GETCOUNT, None, None) }
        .0
        .max(0);
    let overflow = count > (height / row_height) as isize;
    let bar_left = right
        - if overflow {
            dropdown_scrollbar::thickness(state.dpi)
        } else {
            0
        };
    unsafe {
        let _ = SetWindowPos(
            state.batch.list,
            None,
            s(CONTENT_LEFT + 6),
            s(LIST_TOP + 6),
            bar_left - s(CONTENT_LEFT + 6),
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        SendMessageW(
            state.batch.list,
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(row_height as isize)),
        );
        if !overflow {
            SendMessageW(state.batch.list, LB_SETTOPINDEX, Some(WPARAM(0)), None);
        }
    }
    dropdown_scrollbar::position_rect(
        state.batch.scrollbar,
        RECT {
            left: right - dropdown_scrollbar::thickness(state.dpi),
            top: s(LIST_TOP + 6),
            right,
            bottom: s(LIST_TOP + LIST_HEIGHT - 6),
        },
        state.dpi,
    );
    unsafe {
        let _ = ShowWindow(
            state.batch.scrollbar,
            if overflow && GROUPS[state.active_group].0 == GROUP {
                SW_SHOWNOACTIVATE
            } else {
                SW_HIDE
            },
        );
    }
}

pub fn read(state: &SettingsState) -> Result<BatchTranscriptionConfig, String> {
    let batch_size = read_text(state.controls["__batch_size"])
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| state.language.text("batch_invalid_size").to_owned())?;
    let output_dir = read_text(state.controls["__batch_output"])
        .trim()
        .to_owned();
    if output_dir.is_empty() {
        return Ok(BatchTranscriptionConfig {
            output_dir,
            batch_size,
        });
    }
    let path = PathBuf::from(&output_dir);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    Ok(BatchTranscriptionConfig {
        output_dir: path.to_string_lossy().into_owned(),
        batch_size,
    })
}

pub fn sync_controls(state: &SettingsState) {
    let running = state.batch.queue.running;
    state.batch.activity.set(running);
    unsafe {
        for key in ["__batch_output", "__batch_size"] {
            if let Some(hwnd) = state.controls.get(key) {
                let _ = EnableWindow(*hwnd, !state.saving && !running);
            }
        }
        for key in ["batch_files", "batch_folders"] {
            if let Some(hwnd) = state.controls.get(key) {
                let _ = EnableWindow(*hwnd, !state.saving && !state.batch.close_requested);
            }
        }
        if let Some(hwnd) = state.controls.get("batch_transcribe") {
            let _ = EnableWindow(*hwnd, !state.saving && !state.batch.close_requested);
            let _ = SetWindowTextW(
                *hwnd,
                PCWSTR(
                    wide(state.language.text(if running {
                        "cancel"
                    } else {
                        "batch_transcribe"
                    }))
                    .as_ptr(),
                ),
            );
            let _ = InvalidateRect(Some(*hwnd), None, false);
        }
        if let Some(hwnd) = state.controls.get("batch_retry") {
            let _ = EnableWindow(
                *hwnd,
                !state.saving && !state.batch.close_requested && state.batch.queue.can_retry(),
            );
            let _ = InvalidateRect(Some(*hwnd), None, false);
        }
        if let Some(hwnd) = state.controls.get("MAX_UPLOAD_CONCURRENCY") {
            let _ = EnableWindow(*hwnd, !state.saving && !running);
        }
        if let Some(hwnd) = state.controls.get("__save") {
            let _ = EnableWindow(*hwnd, !state.saving && !running);
        }
    }
}

pub fn command(state: &mut SettingsState, id: usize, notification: u32) -> bool {
    if !(ID_OUTPUT..=ID_RETRY).contains(&id) {
        return false;
    }
    if matches!(notification, EN_SETFOCUS | EN_KILLFOCUS | EN_CHANGE) {
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), None, false);
        }
    }
    if notification != BN_CLICKED || state.saving {
        return true;
    }
    match id {
        ID_FILES | ID_FOLDERS => match choose_paths(state.hwnd, id == ID_FOLDERS, state.language) {
            Ok(paths) if !paths.is_empty() => import_paths(state, paths),
            Ok(_) => {}
            Err(error) => show_error(state.hwnd, &error),
        },
        ID_TRANSCRIBE if state.batch.queue.running => {
            state.batch.cancellation.cancel();
        }
        ID_TRANSCRIBE | ID_RETRY => start(state, id == ID_RETRY),
        _ => {}
    }
    true
}

fn start(state: &mut SettingsState, retry: bool) {
    if state.batch.queue.running
        || state.batch.queue.rows.is_empty()
        || (retry && !state.batch.queue.can_retry())
    {
        return;
    }
    let mut config = match read_config(state)
        .and_then(|config| config.validate().map(|_| config).map_err(|e| e.to_string()))
    {
        Ok(config) => config,
        Err(error) => {
            show_error(state.hwnd, &error);
            return;
        }
    };
    if config.batch_transcription.output_dir.is_empty() {
        show_error(state.hwnd, state.language.text("batch_output_required"));
        return;
    }
    if !state.runtime.can_reload() {
        show_error(state.hwnd, state.language.text("batch_busy"));
        return;
    }
    let applied = state.runtime.config();
    config.ffmpeg_debug = applied.ffmpeg_debug;
    config.record_debug = applied.record_debug;
    config.hotkey_debug = applied.hotkey_debug;
    config.upload_debug = applied.upload_debug;
    let jobs = state.batch.queue.begin(retry);
    if jobs.is_empty() {
        return;
    }
    state.batch.cancellation = tokio_util::sync::CancellationToken::new();
    state.batch.control = BatchControl::default();
    let control = state.batch.control.clone();
    let cancellation = state.batch.cancellation.clone();
    let runtime = state.runtime.clone();
    let (tx, rx) = mpsc::channel();
    state.batch.receiver = Some(rx);
    state.batch.animation_started = Some(Instant::now());
    sync_animation_timer(state);
    update_input_controls(state);
    unsafe {
        let _ = InvalidateRect(Some(state.batch.list), None, false);
    }
    std::thread::spawn(move || {
        let result = (|| {
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            executor
                .block_on(runtime.transcribe_batch_controlled(
                    config,
                    jobs,
                    cancellation,
                    control,
                    |update| {
                        let _ = tx.send(WorkerEvent::Update(update));
                    },
                ))
                .map_err(|e| e.to_string())
        })();
        let _ = tx.send(WorkerEvent::Finished(result));
    });
}

fn import_paths(state: &mut SettingsState, paths: Vec<PathBuf>) {
    if state.saving || state.batch.close_requested {
        return;
    }
    let cancellation = state.batch.import_cancellation.child_token();
    let (tx, rx) = mpsc::channel();
    state.batch.imports.push(rx);
    std::thread::spawn(move || {
        let _ = tx.send(collect_files(&paths, &cancellation));
    });
}

pub fn poll(state: &mut SettingsState) {
    let mut finished = None;
    if let Some(receiver) = &state.batch.receiver {
        // Bound a tick's work, keeping painting and Cancel responsive for large queues.
        for _ in 0..200 {
            match receiver.try_recv() {
                Ok(WorkerEvent::Update(update)) => {
                    if let Some(index) = state.batch.queue.update(update) {
                        invalidate_row(&state.batch, index);
                    }
                }
                Ok(WorkerEvent::Finished(result)) => {
                    finished = Some((result, true));
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = Some((
                        Err(state.language.text("batch_worker_stopped").into()),
                        false,
                    ));
                    break;
                }
            }
        }
    }
    if let Some((result, orderly)) = finished {
        state.batch.receiver = None;
        if orderly && result.is_err() {
            state.batch.queue.reject();
        } else {
            state.batch.queue.finish();
        }
        unsafe {
            let _ = InvalidateRect(Some(state.batch.list), None, false);
        }
        update_input_controls(state);
        if let Err(error) = result
            && !state.batch.close_requested
        {
            show_error(state.hwnd, &error);
        }
    }
    let mut import_errors = Vec::new();
    state
        .batch
        .imports
        .retain(|receiver| match receiver.try_recv() {
            Ok(result) => {
                state.batch.additions.extend(result.paths);
                import_errors.extend(result.errors);
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
    let mut added = false;
    for _ in 0..100 {
        let Some(path) = state.batch.additions.pop_front() else {
            break;
        };
        if state.batch.queue.add(path) {
            let row = state.batch.queue.rows.last().unwrap();
            let name = row
                .job
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            unsafe {
                SendMessageW(
                    state.batch.list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(&name).as_ptr() as isize)),
                );
            }
            added = true;
        }
    }
    if added {
        resize(state);
        sync_controls(state);
    }
    if !import_errors.is_empty() && !state.batch.close_requested {
        show_error(
            state.hwnd,
            &format!(
                "{}\n{}",
                state.language.text("batch_import_errors"),
                import_errors
                    .into_iter()
                    .take(8)
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        );
    }
    sync_animation_timer(state);
    update_tooltip(state);
    if state.batch.close_requested && !state.batch.queue.running {
        unsafe {
            let _ = DestroyWindow(state.hwnd);
        }
    }
}

pub fn request_close(state: &mut SettingsState) {
    if state.saving {
        return;
    }
    state.batch.import_cancellation.cancel();
    if state.batch.queue.running {
        state.batch.close_requested = true;
        state.batch.cancellation.cancel();
        sync_controls(state);
    } else {
        unsafe {
            let _ = DestroyWindow(state.hwnd);
        }
    }
}

pub fn destroy(state: &mut SettingsState) {
    state.batch.cancellation.cancel();
    state.batch.import_cancellation.cancel();
    unsafe {
        let _ = KillTimer(Some(state.hwnd), TIMER);
        let _ = KillTimer(Some(state.batch.list), ANIMATION_TIMER);
        if !state.batch.tooltip.is_invalid() {
            let _ = DestroyWindow(state.batch.tooltip);
        }
    }
}

pub fn draw(state: &SettingsState, item: &DRAWITEMSTRUCT) -> bool {
    if item.CtlID as usize == ID_LIST {
        unsafe {
            dropdown::with_buffered_item(item, |buffered| draw_file_row(state, buffered));
        }
        return true;
    }
    if !(ID_FILES..=ID_RETRY).contains(&(item.CtlID as usize)) {
        return false;
    }
    if item.CtlID as usize == ID_TRANSCRIBE && state.batch.queue.running {
        unsafe {
            rounded_box(
                item.hDC,
                item.rcItem,
                if item.itemState.0 & ODS_SELECTED.0 != 0 {
                    rgb(159, 43, 44)
                } else if dropdown::hovered(item.hwndItem) {
                    rgb(208, 60, 61)
                } else {
                    rgb(191, 51, 52)
                },
                if item.itemState.0 & ODS_FOCUS.0 != 0 {
                    rgb(255, 170, 170)
                } else {
                    rgb(230, 90, 90)
                },
                platform::scale(7, state.dpi),
            );
            draw_text_line(
                item.hDC,
                state.font,
                state.language.text("cancel"),
                rgb(255, 245, 245),
                item.rcItem,
                DT_CENTER,
            );
        }
    } else {
        unsafe {
            draw_action_button(item, state.font, state.dpi);
        }
    }
    true
}

fn draw_file_row(state: &SettingsState, item: &DRAWITEMSTRUCT) -> LRESULT {
    let Some(row) = state.batch.queue.rows.get(item.itemID as usize) else {
        return LRESULT(1);
    };
    dropdown::draw_row(
        item.hDC,
        item.rcItem,
        item.itemState.0 & ODS_SELECTED.0 != 0,
        dropdown::hovered_row(item.hwndItem, item.itemID),
        state.dpi,
    );
    let s = |value| platform::scale(value, state.dpi);
    let mut title = item.rcItem;
    title.left += s(10);
    title.right -= s(10);
    let status = match &row.status {
        FileStatus::Pending => None,
        FileStatus::Processing => Some(("batch_processing", rgb(51, 69, 75))),
        FileStatus::Completed { .. } => Some(("batch_completed", rgb(30, 73, 57))),
        FileStatus::Failed(_) => Some(("batch_failed", rgb(87, 48, 49))),
    };
    if let Some((key, fill)) = status {
        let badge = status_badge(item.rcItem, state.dpi);
        title.right = badge.left - s(8);
        unsafe {
            rounded_box(item.hDC, badge, fill, fill, s(7));
            let content = status_content(
                badge,
                label_width(item.hDC, state.font, state.language.text(key), state.dpi),
                state.dpi,
            );
            let text = RECT {
                left: content.text_left,
                right: content.text_right,
                ..badge
            };
            draw_text_line(
                item.hDC,
                state.font,
                state.language.text(key),
                rgb(234, 242, 243),
                text,
                DT_CENTER,
            );
            draw_status_icon(state, item.hDC, content, &row.status);
        }
    }
    unsafe {
        draw_text_line(
            item.hDC,
            state.font,
            &row.job
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            rgb(230, 240, 240),
            title,
            DT_LEFT,
        );
    }
    LRESULT(1)
}

fn status_badge(row: RECT, dpi: u32) -> RECT {
    let s = |value| platform::scale(value, dpi);
    let right = row.right - s(10);
    RECT {
        left: right - s(152),
        top: row.top + s(5),
        right,
        bottom: row.bottom - s(5),
    }
}

fn status_content(badge: RECT, text_width: i32, dpi: u32) -> StatusContent {
    StatusContent::centered(
        [badge.left, badge.top, badge.right, badge.bottom],
        text_width,
        dpi,
    )
}

fn label_width(hdc: HDC, font: HFONT, text: &str, dpi: u32) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let text: Vec<_> = text.encode_utf16().collect();
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(font.0));
        let mut size = SIZE::default();
        let measured = GetTextExtentPoint32W(hdc, &text, &mut size).as_bool();
        SelectObject(hdc, old);
        if measured {
            size.cx.max(0)
        } else {
            text.len() as i32 * platform::scale(8, dpi)
        }
    }
}

fn sync_animation_timer(state: &mut SettingsState) {
    let running = state.batch.queue.running
        && GROUPS[state.active_group].0 == GROUP
        && unsafe { IsWindowVisible(state.batch.list) }.as_bool();
    if running && !state.batch.animation_timer_running {
        state.batch.animation_timer_running = unsafe {
            SetTimer(
                Some(state.batch.list),
                ANIMATION_TIMER,
                test_status_icons::FRAME_INTERVAL_MS,
                None,
            ) != 0
        };
    } else if !running && state.batch.animation_timer_running {
        unsafe {
            let _ = KillTimer(Some(state.batch.list), ANIMATION_TIMER);
        }
        state.batch.animation_timer_running = false;
    }
}

fn invalidate_processing_icons(state: &SettingsState) {
    if GROUPS[state.active_group].0 != GROUP
        || !unsafe { IsWindowVisible(state.batch.list) }.as_bool()
    {
        return;
    }
    unsafe {
        let list = state.batch.list;
        let mut client = RECT::default();
        if GetClientRect(list, &mut client).is_err() {
            return;
        }
        let first = SendMessageW(list, LB_GETTOPINDEX, None, None).0.max(0) as usize;
        let row_height = SendMessageW(list, LB_GETITEMHEIGHT, Some(WPARAM(0)), None)
            .0
            .max(1);
        let visible = (client.bottom.max(0) as usize / row_height as usize) + 2;
        let rows = state
            .batch
            .queue
            .rows
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .filter(|(_, row)| row.status == FileStatus::Processing)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return;
        }
        let dc = GetDC(Some(list));
        if dc.is_invalid() {
            return;
        }
        let width = label_width(
            dc,
            state.font,
            state.language.text("batch_processing"),
            state.dpi,
        );
        ReleaseDC(Some(list), dc);
        for index in rows {
            let mut row = RECT::default();
            if SendMessageW(
                list,
                LB_GETITEMRECT,
                Some(WPARAM(index)),
                Some(LPARAM((&mut row as *mut RECT) as isize)),
            )
            .0 == LB_ERR as isize
                || row.bottom <= 0
                || row.top >= client.bottom
            {
                continue;
            }
            let [left, top, right, bottom] =
                status_content(status_badge(row, state.dpi), width, state.dpi).icon_rect();
            let damage = RECT {
                left,
                top,
                right,
                bottom,
            };
            let _ = InvalidateRect(Some(list), Some(&damage), false);
        }
    }
}

pub fn refresh_language(state: &SettingsState) {
    sync_controls(state);
    unsafe {
        let _ = InvalidateRect(Some(state.batch.list), None, false);
    }
}

fn draw_status_icon(state: &SettingsState, hdc: HDC, content: StatusContent, status: &FileStatus) {
    let icon = match status {
        FileStatus::Processing => StatusIcon::Processing,
        FileStatus::Completed { .. } => StatusIcon::BatchCompleted,
        FileStatus::Failed(_) => StatusIcon::BatchFailed,
        FileStatus::Pending => return,
    };
    let elapsed = state
        .batch
        .animation_started
        .map(|started| started.elapsed())
        .unwrap_or_default();
    let pixels = test_status_icons::pixels(icon, content.icon_size.max(1) as u32, elapsed);
    let _ = crate::prompt_icons::draw_gdi(hdc, content.icon_left, content.icon_top, &pixels);
}

fn invalidate_row(page: &Page, index: usize) {
    unsafe {
        let mut rect = RECT::default();
        if SendMessageW(
            page.list,
            LB_GETITEMRECT,
            Some(WPARAM(index)),
            Some(LPARAM((&mut rect as *mut RECT) as isize)),
        )
        .0 != LB_ERR as isize
        {
            let _ = InvalidateRect(Some(page.list), Some(&rect), false);
        }
    }
}

fn remove_selected(state: &mut SettingsState) {
    if state.saving || state.batch.close_requested {
        return;
    }
    let list = state.batch.list;
    let selected = selected_rows(list);
    if selected.is_empty() {
        return;
    }
    let removed = state.batch.queue.remove(&selected);
    if state.batch.queue.running {
        state.batch.control.remove_files(&removed);
    }
    remove_list_rows(list, &selected);
    resize(state);
    update_tooltip(state);
    update_input_controls(state);
    unsafe {
        let _ = InvalidateRect(Some(list), None, false);
        let _ = InvalidateRect(Some(state.batch.scrollbar), None, false);
    }
}

fn selected_rows(list: HWND) -> Vec<usize> {
    unsafe {
        let count = SendMessageW(list, LB_GETSELCOUNT, None, None).0;
        if count <= 0 {
            return Vec::new();
        }
        let mut indices = vec![0_i32; count as usize];
        let read = SendMessageW(
            list,
            LB_GETSELITEMS,
            Some(WPARAM(indices.len())),
            Some(LPARAM(indices.as_mut_ptr() as isize)),
        )
        .0;
        if read <= 0 {
            return Vec::new();
        }
        indices.truncate(read as usize);
        let mut selected: Vec<_> = indices
            .into_iter()
            .filter_map(|index| usize::try_from(index).ok())
            .collect();
        selected.sort_unstable();
        selected.dedup();
        selected
    }
}

fn remove_list_rows(list: HWND, selected: &[usize]) {
    if selected.is_empty() {
        return;
    }
    unsafe {
        let top = SendMessageW(list, LB_GETTOPINDEX, None, None).0.max(0) as usize;
        for &index in selected.iter().rev() {
            SendMessageW(list, LB_DELETESTRING, Some(WPARAM(index)), None);
        }
        let count = SendMessageW(list, LB_GETCOUNT, None, None).0;
        if count > 0 {
            let next = selected[0].min(count as usize - 1);
            SendMessageW(
                list,
                LB_SETSEL,
                Some(WPARAM(1)),
                Some(LPARAM(next as isize)),
            );
            SendMessageW(list, LB_SETANCHORINDEX, Some(WPARAM(next)), None);
            SendMessageW(list, LB_SETCARETINDEX, Some(WPARAM(next)), Some(LPARAM(0)));
            let top = top
                .saturating_sub(selected.iter().filter(|&&index| index < top).count())
                .min(count as usize - 1);
            let mut client = RECT::default();
            let _ = GetClientRect(list, &mut client);
            let row_height = SendMessageW(list, LB_GETITEMHEIGHT, Some(WPARAM(0)), None)
                .0
                .max(1) as usize;
            let page = ((client.bottom - client.top).max(1) as usize / row_height).max(1);
            // Keep the new selection visible when the removed selection spanned offscreen rows.
            let top = top.min(next).max(next.saturating_sub(page - 1));
            SendMessageW(list, LB_SETTOPINDEX, Some(WPARAM(top)), None);
        }
    }
}

unsafe extern "system" fn list_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    owner: usize,
) -> LRESULT {
    unsafe {
        if message == WM_TIMER && wparam.0 == ANIMATION_TIMER {
            let pointer =
                GetWindowLongPtrW(HWND(owner as *mut c_void), GWLP_USERDATA) as *mut SettingsState;
            if !pointer.is_null() {
                let state = &mut *pointer;
                sync_animation_timer(state);
                if state.batch.animation_timer_running {
                    invalidate_processing_icons(state);
                }
            } else {
                let _ = KillTimer(Some(hwnd), ANIMATION_TIMER);
            }
            return LRESULT(0);
        }
        if wparam.0 == VK_DELETE.0 as usize && matches!(message, WM_KEYDOWN | WM_KEYUP) {
            if message == WM_KEYDOWN && lparam.0 as usize & (1 << 30) == 0 {
                let pointer = GetWindowLongPtrW(HWND(owner as *mut c_void), GWLP_USERDATA)
                    as *mut SettingsState;
                if !pointer.is_null() {
                    remove_selected(&mut *pointer);
                }
            }
            return LRESULT(0);
        }
        if message == WM_DROPFILES {
            let drop = HDROP(wparam.0 as *mut c_void);
            let count = DragQueryFileW(drop, u32::MAX, None);
            let mut paths = Vec::new();
            for index in 0..count {
                let length = DragQueryFileW(drop, index, None) as usize;
                let mut text = vec![0; length + 1];
                DragQueryFileW(drop, index, Some(&mut text));
                use std::os::windows::ffi::OsStringExt;
                paths.push(PathBuf::from(std::ffi::OsString::from_wide(
                    &text[..length],
                )));
            }
            DragFinish(drop);
            let pointer =
                GetWindowLongPtrW(HWND(owner as *mut c_void), GWLP_USERDATA) as *mut SettingsState;
            if !pointer.is_null() {
                import_paths(&mut *pointer, paths);
            }
            return LRESULT(0);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

fn choose_paths(owner: HWND, folders: bool, language: Language) -> Result<Vec<PathBuf>, String> {
    let result = || -> windows::core::Result<Vec<PathBuf>> {
        unsafe {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            let mut options = dialog.GetOptions()?
                | FOS_ALLOWMULTISELECT
                | FOS_FORCEFILESYSTEM
                | FOS_PATHMUSTEXIST
                | FOS_NOCHANGEDIR;
            options |= if folders {
                FOS_PICKFOLDERS
            } else {
                FOS_FILEMUSTEXIST
            };
            dialog.SetOptions(options)?;
            dialog.SetTitle(PCWSTR(
                wide(language.text(if folders {
                    "batch_folders"
                } else {
                    "batch_files"
                }))
                .as_ptr(),
            ))?;
            if !folders {
                let filter = wide(language.text("batch_audio_files"));
                let patterns = wide(
                    &AUDIO_FILE_EXTENSIONS
                        .iter()
                        .map(|extension| format!("*.{extension}"))
                        .collect::<Vec<_>>()
                        .join(";"),
                );
                dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                    pszName: PCWSTR(filter.as_ptr()),
                    pszSpec: PCWSTR(patterns.as_ptr()),
                }])?;
            }
            match dialog.Show(Some(owner)) {
                Ok(()) => {}
                Err(error)
                    if error.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) =>
                {
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            }
            let items = dialog.GetResults()?;
            let mut paths = Vec::new();
            for index in 0..items.GetCount()? {
                let value = items.GetItemAt(index)?.GetDisplayName(SIGDN_FILESYSPATH)?;
                paths.push(take_shell_path(value));
            }
            Ok(paths)
        }
    };
    result().map_err(|error| error.to_string())
}

unsafe fn take_shell_path(value: PWSTR) -> PathBuf {
    unsafe {
        use std::os::windows::ffi::OsStringExt;
        let result = PathBuf::from(std::ffi::OsString::from_wide(value.as_wide()));
        CoTaskMemFree(Some(value.0.cast()));
        result
    }
}

fn default_output_directory() -> Result<PathBuf, String> {
    unsafe {
        Ok(take_shell_path(
            SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DONT_VERIFY, None)
                .map_err(|e| e.to_string())?,
        )
        .join("Transcripts"))
    }
}

fn create_tooltip(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    unsafe {
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        })
        .ok()
        .map_err(|e| e.to_string())?;
        let tooltip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            w!(""),
            WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
            0,
            0,
            0,
            0,
            Some(state.hwnd),
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        state.batch.tooltip = tooltip;
        state.batch.tooltip_text = wide("");
        let info = tooltip_info(state);
        SendMessageW(
            tooltip,
            TTM_ADDTOOLW,
            None,
            Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
        );
        SendMessageW(
            tooltip,
            TTM_SETMAXTIPWIDTH,
            None,
            Some(LPARAM(platform::scale(500, state.dpi) as isize)),
        );
    }
    Ok(())
}

fn tooltip_info(state: &SettingsState) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd: state.hwnd,
        uId: state.batch.list.0 as usize,
        lpszText: PWSTR(state.batch.tooltip_text.as_ptr() as *mut u16),
        ..Default::default()
    }
}

fn update_tooltip(state: &mut SettingsState) {
    if state.batch.tooltip.is_invalid() || GROUPS[state.active_group].0 != GROUP {
        return;
    }
    unsafe {
        let mut cursor = POINT::default();
        if GetCursorPos(&mut cursor).is_err()
            || !ScreenToClient(state.batch.list, &mut cursor).as_bool()
        {
            return;
        }
        let hit = SendMessageW(
            state.batch.list,
            LB_ITEMFROMPOINT,
            None,
            Some(LPARAM(
                ((cursor.y as u16 as u32) << 16 | cursor.x as u16 as u32) as isize,
            )),
        )
        .0 as usize;
        let text = if hit >> 16 == 0 {
            state
                .batch
                .queue
                .rows
                .get(hit & 0xffff)
                .map(|row| {
                    let detail = match &row.status {
                        FileStatus::Failed(error) => error.clone(),
                        FileStatus::Completed { output: Some(path) } => path.display().to_string(),
                        FileStatus::Completed { output: None } => {
                            state.language.text("No speech detected").into()
                        }
                        _ => String::new(),
                    };
                    format!("{}\n{}", row.job.path.display(), detail)
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        let text = wide(&text);
        if text != state.batch.tooltip_text {
            state.batch.tooltip_text = text;
            let info = tooltip_info(state);
            SendMessageW(
                state.batch.tooltip,
                TTM_UPDATETIPTEXTW,
                None,
                Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
            );
        }
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    struct TestWindow(HWND);
    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    // Native coverage: compiled by cross-target checks, executed only on Windows.
    #[test]
    fn extended_selection_and_deletion_keep_list_rows_aligned() {
        unsafe {
            let parent = TestWindow(
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    300,
                    200,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            let list = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LISTBOX"),
                w!(""),
                WS_CHILD | WINDOW_STYLE(LIST_OPTIONS as u32),
                0,
                0,
                300,
                200,
                Some(parent.0),
                None,
                None,
                None,
            )
            .unwrap();
            SendMessageW(list, LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(36)));
            for index in 0..5 {
                SendMessageW(
                    list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(&format!("file{index}.wav")).as_ptr() as isize)),
                );
            }
            for index in [0, 2, 4] {
                SendMessageW(list, LB_SETSEL, Some(WPARAM(1)), Some(LPARAM(index)));
            }
            assert_eq!(selected_rows(list), vec![0, 2, 4]);
            remove_list_rows(list, &selected_rows(list));
            assert_eq!(SendMessageW(list, LB_GETCOUNT, None, None).0, 2);
            for (name, index) in [("file1.wav", 0), ("file3.wav", 1)] {
                assert_eq!(
                    SendMessageW(
                        list,
                        LB_FINDSTRINGEXACT,
                        Some(WPARAM(usize::MAX)),
                        Some(LPARAM(wide(name).as_ptr() as isize))
                    )
                    .0,
                    index
                );
            }
            assert_eq!(selected_rows(list), vec![0]);
            assert_eq!(SendMessageW(list, LB_GETCARETINDEX, None, None).0, 0);
            assert_eq!(SendMessageW(list, LB_GETANCHORINDEX, None, None).0, 0);
            remove_list_rows(list, &selected_rows(list));
            assert_eq!(SendMessageW(list, LB_GETCOUNT, None, None).0, 1);
            for name in ["extra0.wav", "extra1.wav"] {
                SendMessageW(
                    list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(name).as_ptr() as isize)),
                );
            }
            SendMessageW(list, LB_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            SendMessageW(list, LB_SELITEMRANGEEX, Some(WPARAM(0)), Some(LPARAM(2)));
            assert_eq!(selected_rows(list), vec![0, 1, 2]);
            remove_list_rows(list, &selected_rows(list));
            assert_eq!(SendMessageW(list, LB_GETCOUNT, None, None).0, 0);
            assert!(selected_rows(list).is_empty());
        }
    }
}
