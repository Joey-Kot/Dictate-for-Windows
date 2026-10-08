//! Manual Advanced Audio API workflow settings.
//!
//! The page deliberately keeps the workflow separate from its values and
//! secrets.  It retains remote-audio configuration owned by later pages while
//! editing the shared Advanced Audio API config.

use super::*;
use crate::advanced_audio_prompt::{self, CompilerOutput};
use dictate_core::advanced_audio::schema::{PollCondition, ResponseExtractor, StreamAction};
use dictate_core::advanced_audio::{
    AdvancedAudioConfig, AdvancedAudioWorkflow, AdvancedRecognition, AudioDelivery, HttpStage,
    ParameterOption, ParameterType, RemoteAudioConfig, VisibilityCondition,
    validate_remote_audio_config, validate_workflow,
};
use std::collections::BTreeSet;
use std::mem::size_of;
use windows::Win32::UI::Controls::{
    ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, NMHDR, NMTTDISPINFOW,
    TOOLTIPS_CLASSW, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_NEWTOOLRECTW, TTM_POP, TTM_SETMAXTIPWIDTH,
    TTM_SETTIPBKCOLOR, TTM_SETTIPTEXTCOLOR, TTN_GETDISPINFOW, TTS_ALWAYSTIP, TTS_NOPREFIX,
    TTS_USEVISUALSTYLE, TTTOOLINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, SetFocus, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetDlgCtrlID, GetNextDlgTabItem, GetParent, HWND_TOPMOST, LB_ADDSTRING, LB_GETCURSEL,
    LB_GETSEL, LB_ITEMFROMPOINT, LB_RESETCONTENT, LB_SETCURSEL, LB_SETITEMHEIGHT, LB_SETSEL,
    LBN_SELCHANGE, LBS_HASSTRINGS, LBS_MULTIPLESEL, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY,
    LBS_OWNERDRAWFIXED, PostMessageW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetParent, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONUP, WM_MOUSEWHEEL, WM_NCDESTROY, WM_NOTIFY,
    WM_SETFOCUS, WS_EX_CONTROLPARENT,
};
use windows::core::PWSTR;

pub(super) const GROUP: &str = "Advanced";
pub(super) const TIMER: usize = 0x6c09;

const ID_ENABLE: usize = 0x6c00;
const ID_WORKFLOW: usize = 0x6c01;
const ID_VALIDATE: usize = 0x6c04;
const ID_TEST: usize = 0x6c05;
const ID_MATERIAL: usize = 0x6c06;
const ID_GENERATE: usize = 0x6c07;
const ID_CANCEL_GENERATION: usize = 0x6c08;
const ID_REMOTE_OPEN: usize = 0x6c0a;
const ID_REMOTE_BACK: usize = 0x6c0b;
const ID_REMOTE_APPLY: usize = 0x6c0c;
const ID_REMOTE_NONE: usize = 0x6c0d;
const ID_REMOTE_WEBDAV: usize = 0x6c0e;
const ID_REMOTE_S3: usize = 0x6c0f;
const ID_REMOTE_OSS: usize = 0x6c10;
const ID_REMOTE_PRESIGNED: usize = 0x6c11;
const ID_REMOTE_DELETE_AFTER: usize = 0x6c12;
const ID_DYNAMIC_VALUE: usize = 0x6c13;
const ID_DYNAMIC_SECRET: usize = 0x6c14;
const ID_DYNAMIC_PREVIOUS: usize = 0x6c15;
const ID_DYNAMIC_NEXT: usize = 0x6c16;
const ID_SUMMARY: usize = 0x6c17;
const ID_RESET: usize = 0x6c18;
const ID_GENERATION_STATUS: usize = 0x6c19;
const ID_USER_REQUIREMENTS: usize = 0x6c1a;
const ID_DYNAMIC_BOOLEAN: usize = 0x6c1b;
const ID_DYNAMIC_SELECT: usize = 0x6c1c;
const ID_DYNAMIC_MULTI_SELECT: usize = 0x6c1d;
const ID_DYNAMIC_JSON_OBJECT: usize = 0x6c1e;
const ID_DYNAMIC_JSON_ARRAY: usize = 0x6c1f;
const ID_REMOTE_FIELD_BASE: usize = 0x6c20;
const ID_DYNAMIC_SELECT_LIST: usize = 0x6c40;
const ID_DYNAMIC_MULTI_SELECT_LIST: usize = 0x6c41;
const ID_DYNAMIC_VALUE_LABEL_TOOLTIP: usize = 0x6c42;
const ID_DYNAMIC_SECRET_LABEL_TOOLTIP: usize = 0x6c43;
const REMOTE_VIEWPORT_SUBCLASS_ID: usize = 0x6c01;
const REMOTE_CONTROL_SUBCLASS_ID: usize = 0x6c02;
const REMOTE_REVEAL: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 34;
const DYNAMIC_SELECT_ACTION: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 35;
const DYNAMIC_MULTI_SELECT_ACTION: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 36;
const DYNAMIC_MULTI_SELECT_SYNC: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 37;
const DYNAMIC_SELECT_PICK: usize = 1;
const DYNAMIC_SELECT_CLOSE: usize = 2;
const DYNAMIC_SELECT_CLOSE_IF_OUTSIDE: usize = 3;
const DYNAMIC_SELECT_TOGGLE: usize = 4;
const DYNAMIC_SELECT_TAB: usize = 5;

const ENABLE_KEY: &str = "__advanced_audio_enabled";
const WORKFLOW_KEY: &str = "__advanced_audio_workflow";
const VALIDATE_KEY: &str = "__advanced_audio_validate";
const TEST_KEY: &str = "__advanced_audio_test";
const MATERIAL_KEY: &str = "__advanced_audio_material";
const USER_REQUIREMENTS_KEY: &str = "__advanced_audio_user_requirements";
const DYNAMIC_BOOLEAN_KEY: &str = "__advanced_audio_dynamic_boolean";
const GENERATE_KEY: &str = "__advanced_audio_generate";
const CANCEL_GENERATION_KEY: &str = "__advanced_audio_cancel_generation";
const RESET_KEY: &str = "__advanced_audio_reset";
const REMOTE_OPEN_KEY: &str = "__advanced_audio_remote_open";
const REMOTE_BACK_KEY: &str = "__advanced_audio_remote_back";
const REMOTE_APPLY_KEY: &str = "__advanced_audio_remote_apply";
const REMOTE_NONE_KEY: &str = "__advanced_audio_remote_none";
const REMOTE_WEBDAV_KEY: &str = "__advanced_audio_remote_webdav";
const REMOTE_S3_KEY: &str = "__advanced_audio_remote_s3";
const REMOTE_OSS_KEY: &str = "__advanced_audio_remote_oss";
const REMOTE_PRESIGNED_KEY: &str = "__advanced_audio_remote_presigned";
const REMOTE_DELETE_AFTER_KEY: &str = "__advanced_audio_remote_delete_after";
const DYNAMIC_PREVIOUS_KEY: &str = "__advanced_audio_dynamic_previous";
const DYNAMIC_NEXT_KEY: &str = "__advanced_audio_dynamic_next";

const REMOTE_WEBDAV_FIELDS: [(&str, &str, bool); 5] = [
    (
        "__advanced_audio_remote_webdav_upload_base_url",
        "advanced_audio_remote_upload_base_url",
        false,
    ),
    (
        "__advanced_audio_remote_webdav_username",
        "advanced_audio_remote_username",
        false,
    ),
    (
        "__advanced_audio_remote_webdav_password",
        "advanced_audio_remote_password",
        true,
    ),
    (
        "__advanced_audio_remote_webdav_path_prefix",
        "advanced_audio_remote_path_prefix",
        false,
    ),
    (
        "__advanced_audio_remote_webdav_public_download_base_url",
        "advanced_audio_remote_public_download_base_url",
        false,
    ),
];
const REMOTE_S3_FIELDS: [(&str, &str, bool); 7] = [
    (
        "__advanced_audio_remote_s3_endpoint",
        "advanced_audio_remote_endpoint",
        false,
    ),
    (
        "__advanced_audio_remote_s3_region",
        "advanced_audio_remote_region",
        false,
    ),
    (
        "__advanced_audio_remote_s3_bucket",
        "advanced_audio_remote_bucket",
        false,
    ),
    (
        "__advanced_audio_remote_s3_access_key",
        "advanced_audio_remote_access_key",
        false,
    ),
    (
        "__advanced_audio_remote_s3_secret_key",
        "advanced_audio_remote_secret_key",
        true,
    ),
    (
        "__advanced_audio_remote_s3_prefix",
        "advanced_audio_remote_path_prefix",
        false,
    ),
    (
        "__advanced_audio_remote_s3_public_url_base",
        "advanced_audio_remote_public_url_base",
        false,
    ),
];
const REMOTE_OSS_FIELDS: [(&str, &str, bool); 6] = [
    (
        "__advanced_audio_remote_oss_endpoint",
        "advanced_audio_remote_endpoint",
        false,
    ),
    (
        "__advanced_audio_remote_oss_bucket",
        "advanced_audio_remote_bucket",
        false,
    ),
    (
        "__advanced_audio_remote_oss_access_key",
        "advanced_audio_remote_access_key",
        false,
    ),
    (
        "__advanced_audio_remote_oss_secret_key",
        "advanced_audio_remote_secret_key",
        true,
    ),
    (
        "__advanced_audio_remote_oss_prefix",
        "advanced_audio_remote_path_prefix",
        false,
    ),
    (
        "__advanced_audio_remote_oss_public_url_base",
        "advanced_audio_remote_public_url_base",
        false,
    ),
];

const EDIT_LEFT: i32 = CONTENT_LEFT;
const EDIT_WIDTH: i32 = 536;
const GENERATION_INPUT_GAP: i32 = 12;
const GENERATION_INPUT_WIDTH: i32 = (EDIT_WIDTH - GENERATION_INPUT_GAP) / 2;
const MATERIAL_LEFT: i32 = EDIT_LEFT + GENERATION_INPUT_WIDTH + GENERATION_INPUT_GAP;
const MATERIAL_TOP: i32 = 158;
const MATERIAL_HEIGHT: i32 = 60;
const GENERATION_ACTION_TOP: i32 = 228;
const SUMMARY_HEIGHT: i32 = GENERATION_ACTION_TOP + FIELD_HEIGHT - MATERIAL_TOP;
const SUMMARY_RESET_TOP: i32 = 131;
const SUMMARY_RESET_WIDTH: i32 = 120;
const SUMMARY_RESET_HEIGHT: i32 = 24;
const SUMMARY_RESET_GAP: i32 = 8;
const WORKFLOW_LABEL_TOP: i32 = 270;
const WORKFLOW_TOP: i32 = 296;
const WORKFLOW_HEIGHT: i32 = 82;
const VALUES_LABEL_TOP: i32 = 386;
const VALUES_TOP: i32 = 410;
const JSON_EDITOR_HEIGHT: i32 = 76;
// Keep dynamically declared choice rows consistent with the shared language
// and audio selectors while retaining the three-row visible limit.
const MULTI_SELECT_ROW_HEIGHT: i32 = dropdown::ROW_HEIGHT;
const DYNAMIC_SELECT_MAX_ROWS: usize = 3;
const MULTI_SELECT_VISIBLE_ROWS: usize = DYNAMIC_SELECT_MAX_ROWS;
const DYNAMIC_LABEL_TOOLTIP_MAX_WIDTH: i32 = 320;
// Win32 `SS_ENDELLIPSIS`: make the available hover detail discoverable without
// widening the form or shrinking its editor.
const STATIC_STYLE_END_ELLIPSIS: u32 = 0x0000_4000;
const SECRETS_LABEL_TOP: i32 = 458;
const SECRETS_TOP: i32 = 482;
const VALIDATION_ACTION_TOP: i32 = 532;
const STATUS_TOP: i32 = 570;
const GENERATION_STATUS_TOP: i32 = VALUES_TOP;
const GENERATION_STATUS_HEIGHT: i32 = VALIDATION_ACTION_TOP - GENERATION_STATUS_TOP - 10;
const REMOTE_FIELD_TOP: i32 = 208;
const REMOTE_FIELD_STEP: i32 = 42;
const REMOTE_ACTION_BOTTOM_GAP: i32 = 12;
const REMOTE_ACTION_TOP: i32 =
    WINDOW_HEIGHT - FOOTER_HEIGHT - FIELD_HEIGHT - REMOTE_ACTION_BOTTOM_GAP;
const REMOTE_VIEWPORT_HEIGHT: i32 = REMOTE_ACTION_TOP - REMOTE_FIELD_TOP - 10;
const REMOTE_CONTENT_BOTTOM_PADDING: i32 = 12;

pub(super) struct Page {
    config: AdvancedAudioConfig,
    validation: ValidationState,
    generation: GenerationState,
    generation_cancel: tokio_util::sync::CancellationToken,
    generation_receiver: Option<std::sync::mpsc::Receiver<Result<CompilerOutput, String>>>,
    remote_draft: RemoteAudioConfig,
    remote_open: bool,
    normal_controls: Vec<HWND>,
    generation_controls: Vec<HWND>,
    remote_common_controls: Vec<HWND>,
    remote_webdav_controls: Vec<HWND>,
    remote_s3_controls: Vec<HWND>,
    remote_oss_controls: Vec<HWND>,
    remote_presigned_controls: Vec<HWND>,
    remote_cleanup_controls: Vec<HWND>,
    remote_viewport: HWND,
    remote_scrollbar: HWND,
    remote_offset: i32,
    remote_layouts: Vec<RemoteLayout>,
    dynamic_values_heading: HWND,
    dynamic_secrets_heading: HWND,
    dynamic_value_label: HWND,
    dynamic_label_tooltip: HWND,
    dynamic_value_label_tooltip_text: Vec<u16>,
    dynamic_secret_label_tooltip_text: Vec<u16>,
    dynamic_value_editor: HWND,
    dynamic_boolean: HWND,
    dynamic_select: HWND,
    dynamic_select_panel: HWND,
    dynamic_select_list: HWND,
    dynamic_select_scrollbar: HWND,
    dynamic_multi_select: HWND,
    dynamic_multi_select_panel: HWND,
    dynamic_multi_select_list: HWND,
    dynamic_multi_select_scrollbar: HWND,
    // The parameter ID and token make an asynchronously posted selection
    // update harmless if the user switches or rebinds a dynamic page first.
    dynamic_multi_select_sync_pending: Option<DynamicMultiSelectSync>,
    dynamic_multi_select_sync_token: usize,
    dynamic_json_object: HWND,
    dynamic_json_array: HWND,
    dynamic_secret_label: HWND,
    dynamic_secret_editor: HWND,
    dynamic_previous: HWND,
    dynamic_next: HWND,
    dynamic_form: Option<DynamicForm>,
    updating_dynamic_controls: bool,
    summary_label: HWND,
    summary_editor: HWND,
    summary_reset: HWND,
    generation_status: HWND,
    test_requested: bool,
}

enum ValidationState {
    NotValidated,
    Valid(WorkflowSummary),
    Invalid(String),
}

enum GenerationState {
    Idle,
    Generating,
    Canceled,
    NeedsMoreInformation(String),
    Unsupported(String),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RemoteProvider {
    None,
    Webdav,
    S3Compatible,
    AliyunOss,
}

#[derive(Clone, Copy)]
enum RemoteControlGroup {
    Common,
    Webdav,
    S3,
    Oss,
    Presigned,
    Cleanup,
}

#[derive(Clone, Copy)]
struct RemoteLayout {
    control: HWND,
    rect: RECT,
}

struct WorkflowSummary {
    name: Option<String>,
    mode: Option<String>,
    version: Option<String>,
    audio_delivery: String,
    structure: Vec<String>,
    warnings: Vec<String>,
    required_values: Vec<String>,
    required_secrets: Vec<String>,
    test_preview: WorkflowTestPreview,
}

#[derive(Clone)]
struct WorkflowTestPreview {
    targets: Vec<WorkflowTestTarget>,
    remote_upload: bool,
    mode: String,
    realtime_replay: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum WorkflowTestTarget {
    Host(String),
    DynamicHttpUrl,
}

impl WorkflowTestTarget {
    fn display(&self, language: Language) -> String {
        match self {
            Self::Host(host) => host.clone(),
            Self::DynamicHttpUrl => language
                .text("advanced_audio_test_dynamic_http_target")
                .into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct DynamicInput {
    id: String,
    label: String,
    default: Option<String>,
    parameter_type: ParameterType,
    options: Vec<ParameterOption>,
    visible_when: Option<VisibilityCondition>,
}

#[derive(Clone, PartialEq, Eq)]
struct DynamicForm {
    parameters: Vec<DynamicInput>,
    secrets: Vec<DynamicInput>,
    page: usize,
}

#[derive(Clone, Copy)]
struct DynamicPage {
    parameter_index: Option<usize>,
    secret_index: Option<usize>,
}

struct DynamicMultiSelectSync {
    token: usize,
    parameter_id: String,
}

impl DynamicForm {
    fn from_workflow(workflow: &AdvancedAudioWorkflow) -> Self {
        Self {
            parameters: workflow
                .parameters
                .iter()
                .map(|definition| DynamicInput {
                    id: definition.id.clone(),
                    label: declaration_name(&definition.label, &definition.id),
                    default: definition.default.clone(),
                    parameter_type: definition
                        .effective_type(workflow.schema_version)
                        .unwrap_or(ParameterType::Text),
                    options: definition.options.clone(),
                    visible_when: definition.visible_when.clone(),
                })
                .collect(),
            secrets: workflow
                .secrets
                .iter()
                .map(|definition| DynamicInput {
                    id: definition.id.clone(),
                    label: declaration_name(&definition.label, &definition.id),
                    default: None,
                    parameter_type: ParameterType::Text,
                    options: Vec::new(),
                    visible_when: None,
                })
                .collect(),
            page: 0,
        }
    }

    fn visible_parameter_indices(
        &self,
        values: &std::collections::BTreeMap<String, String>,
    ) -> Vec<usize> {
        self.parameters
            .iter()
            .enumerate()
            .filter_map(|(index, definition)| {
                self.parameter_is_visible(definition, values)
                    .then_some(index)
            })
            .collect()
    }

    fn parameter_is_visible(
        &self,
        definition: &DynamicInput,
        values: &std::collections::BTreeMap<String, String>,
    ) -> bool {
        let Some(condition) = &definition.visible_when else {
            return true;
        };
        let value = values
            .get(&condition.parameter)
            .map(String::as_str)
            .or_else(|| {
                self.parameters
                    .iter()
                    .find(|source| source.id == condition.parameter)
                    .and_then(|source| source.default.as_deref())
            })
            .unwrap_or_default();
        condition.equals.as_deref() == Some(value)
            || condition.one_of.iter().any(|candidate| candidate == value)
    }

    fn pages(&self, values: &std::collections::BTreeMap<String, String>) -> Vec<DynamicPage> {
        let parameters = self.visible_parameter_indices(values);
        let mut parameter = 0;
        let mut secret = 0;
        let mut pages = Vec::new();
        while parameter < parameters.len() || secret < self.secrets.len() {
            let parameter_index = parameters.get(parameter).copied();
            if parameter_index.is_some_and(|index| {
                matches!(
                    self.parameters[index].parameter_type,
                    ParameterType::Select
                        | ParameterType::MultiSelect
                        | ParameterType::JsonObject
                        | ParameterType::JsonArray
                )
            }) {
                pages.push(DynamicPage {
                    parameter_index,
                    secret_index: None,
                });
                parameter += 1;
                continue;
            }
            pages.push(DynamicPage {
                parameter_index,
                secret_index: (secret < self.secrets.len()).then_some(secret),
            });
            parameter += usize::from(parameter_index.is_some());
            secret += usize::from(secret < self.secrets.len());
        }
        pages
    }

    fn current_page(
        &self,
        values: &std::collections::BTreeMap<String, String>,
    ) -> Option<DynamicPage> {
        self.pages(values).get(self.page).copied()
    }

    fn normalize_page(&mut self, values: &std::collections::BTreeMap<String, String>) {
        self.page = self.page.min(self.pages(values).len().saturating_sub(1));
    }

    fn has_visibility_dependents(&self, parameter_id: &str) -> bool {
        self.parameters.iter().any(|definition| {
            definition
                .visible_when
                .as_ref()
                .is_some_and(|condition| condition.parameter == parameter_id)
        })
    }
}

impl Page {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            config: config.advanced_audio_api.clone(),
            validation: ValidationState::NotValidated,
            generation: GenerationState::Idle,
            generation_cancel: tokio_util::sync::CancellationToken::new(),
            generation_receiver: None,
            remote_draft: config.advanced_audio_api.remote_audio.clone(),
            remote_open: false,
            normal_controls: Vec::new(),
            generation_controls: Vec::new(),
            remote_common_controls: Vec::new(),
            remote_webdav_controls: Vec::new(),
            remote_s3_controls: Vec::new(),
            remote_oss_controls: Vec::new(),
            remote_presigned_controls: Vec::new(),
            remote_cleanup_controls: Vec::new(),
            remote_viewport: HWND::default(),
            remote_scrollbar: HWND::default(),
            remote_offset: 0,
            remote_layouts: Vec::new(),
            dynamic_values_heading: HWND::default(),
            dynamic_secrets_heading: HWND::default(),
            dynamic_value_label: HWND::default(),
            dynamic_label_tooltip: HWND::default(),
            dynamic_value_label_tooltip_text: vec![0],
            dynamic_secret_label_tooltip_text: vec![0],
            dynamic_value_editor: HWND::default(),
            dynamic_boolean: HWND::default(),
            dynamic_select: HWND::default(),
            dynamic_select_panel: HWND::default(),
            dynamic_select_list: HWND::default(),
            dynamic_select_scrollbar: HWND::default(),
            dynamic_multi_select: HWND::default(),
            dynamic_multi_select_panel: HWND::default(),
            dynamic_multi_select_list: HWND::default(),
            dynamic_multi_select_scrollbar: HWND::default(),
            dynamic_multi_select_sync_pending: None,
            dynamic_multi_select_sync_token: 0,
            dynamic_json_object: HWND::default(),
            dynamic_json_array: HWND::default(),
            dynamic_secret_label: HWND::default(),
            dynamic_secret_editor: HWND::default(),
            dynamic_previous: HWND::default(),
            dynamic_next: HWND::default(),
            dynamic_form: None,
            updating_dynamic_controls: false,
            summary_label: HWND::default(),
            summary_editor: HWND::default(),
            summary_reset: HWND::default(),
            generation_status: HWND::default(),
            test_requested: false,
        }
    }
}

pub(super) fn create(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let enabled = state.advanced_audio.config.enabled;
    let workflow = workflow_editor_text(state.advanced_audio.config.workflow.as_ref());

    let enable_label = create_label(
        state,
        state.language.text("advanced_audio_enable"),
        CONTENT_LEFT,
        94,
        EDIT_WIDTH - 36,
        FIELD_HEIGHT,
        instance,
    )?;
    register_label(state, enable_label, "advanced_audio_enable");
    let enable = create_child(
        state,
        w!("BUTTON"),
        "",
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        CONTENT_LEFT + EDIT_WIDTH - 26,
        99,
        24,
        24,
        ID_ENABLE,
        instance,
    )?;
    state.boolean_values.insert(ENABLE_KEY, enabled);
    state.boolean_ids.insert(ID_ENABLE, ENABLE_KEY);
    state.controls.insert(ENABLE_KEY, enable);
    state.control_groups.insert(enable.0 as usize, GROUP);
    register_normal_control(state, enable);

    create_generation_editor_label(
        state,
        "advanced_audio_user_requirements",
        EDIT_LEFT,
        GENERATION_INPUT_WIDTH,
        instance,
    )?;
    create_editor_at(
        state,
        USER_REQUIREMENTS_KEY,
        ID_USER_REQUIREMENTS,
        "",
        EDIT_LEFT,
        GENERATION_INPUT_WIDTH,
        MATERIAL_TOP,
        MATERIAL_HEIGHT,
        false,
        true,
        instance,
    )?;
    create_generation_editor_label(
        state,
        "advanced_audio_material",
        MATERIAL_LEFT,
        GENERATION_INPUT_WIDTH,
        instance,
    )?;
    create_editor_at(
        state,
        MATERIAL_KEY,
        ID_MATERIAL,
        "",
        MATERIAL_LEFT,
        GENERATION_INPUT_WIDTH,
        MATERIAL_TOP,
        MATERIAL_HEIGHT,
        false,
        true,
        instance,
    )?;

    for (key, id, x, width, label) in [
        (
            GENERATE_KEY,
            ID_GENERATE,
            CONTENT_LEFT,
            132,
            "advanced_audio_generate",
        ),
        (
            CANCEL_GENERATION_KEY,
            ID_CANCEL_GENERATION,
            CONTENT_LEFT + 144,
            132,
            "advanced_audio_cancel_generation",
        ),
    ] {
        let button = create_child(
            state,
            w!("BUTTON"),
            state.language.text(label),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            x,
            GENERATION_ACTION_TOP,
            width,
            FIELD_HEIGHT,
            id,
            instance,
        )?;
        state.controls.insert(key, button);
        state.control_groups.insert(button.0 as usize, GROUP);
        state.localized_controls.push((button, label));
        register_generation_control(state, button);
    }

    create_editor_label(
        state,
        "advanced_audio_workflow_json",
        WORKFLOW_LABEL_TOP,
        false,
        instance,
    )?;
    create_editor(
        state,
        WORKFLOW_KEY,
        ID_WORKFLOW,
        &workflow,
        WORKFLOW_TOP,
        WORKFLOW_HEIGHT,
        false,
        false,
        instance,
    )?;

    create_dynamic_form_controls(state, instance)?;
    create_summary_controls(state, instance)?;
    create_generation_status_control(state, instance)?;

    for (key, id, x, width, label) in [
        (
            VALIDATE_KEY,
            ID_VALIDATE,
            CONTENT_LEFT + 144,
            88,
            "advanced_audio_validate",
        ),
        (
            TEST_KEY,
            ID_TEST,
            CONTENT_LEFT + 240,
            112,
            "advanced_audio_test",
        ),
        (
            REMOTE_OPEN_KEY,
            ID_REMOTE_OPEN,
            CONTENT_LEFT + 360,
            EDIT_WIDTH - 360,
            "advanced_audio_remote_configure",
        ),
    ] {
        let button = create_child(
            state,
            w!("BUTTON"),
            state.language.text(label),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            x,
            VALIDATION_ACTION_TOP,
            width,
            FIELD_HEIGHT,
            id,
            instance,
        )?;
        state.controls.insert(key, button);
        state.control_groups.insert(button.0 as usize, GROUP);
        state.localized_controls.push((button, label));
        register_normal_control(state, button);
    }

    create_remote_editor_controls(state, instance)?;

    refresh_dynamic_form(state);
    update_controls(state);
    Ok(())
}

fn register_label(state: &mut SettingsState, label: HWND, key: &'static str) {
    state.control_groups.insert(label.0 as usize, GROUP);
    state.localized_controls.push((label, key));
    register_normal_control(state, label);
}

fn register_normal_control(state: &mut SettingsState, control: HWND) {
    state.advanced_audio.normal_controls.push(control);
}

fn register_generation_control(state: &mut SettingsState, control: HWND) {
    state.advanced_audio.generation_controls.push(control);
}

fn register_dynamic_control(state: &mut SettingsState, control: HWND) {
    state.control_groups.insert(control.0 as usize, GROUP);
}

fn register_remote_control(state: &mut SettingsState, control: HWND, group: RemoteControlGroup) {
    let page = &mut state.advanced_audio;
    match group {
        RemoteControlGroup::Common => page.remote_common_controls.push(control),
        RemoteControlGroup::Webdav => page.remote_webdav_controls.push(control),
        RemoteControlGroup::S3 => page.remote_s3_controls.push(control),
        RemoteControlGroup::Oss => page.remote_oss_controls.push(control),
        RemoteControlGroup::Presigned => page.remote_presigned_controls.push(control),
        RemoteControlGroup::Cleanup => page.remote_cleanup_controls.push(control),
    }
}

fn create_editor_label(
    state: &mut SettingsState,
    key: &'static str,
    y: i32,
    generation: bool,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    create_editor_label_at(state, key, EDIT_LEFT, EDIT_WIDTH, y, generation, instance)
}

fn create_generation_editor_label(
    state: &mut SettingsState,
    key: &'static str,
    left: i32,
    width: i32,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    create_editor_label_at(state, key, left, width, 132, true, instance)
}

#[allow(clippy::too_many_arguments)]
fn create_editor_label_at(
    state: &mut SettingsState,
    key: &'static str,
    left: i32,
    width: i32,
    y: i32,
    generation: bool,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let label = create_label(
        state,
        state.language.text(key),
        left,
        y,
        width,
        22,
        instance,
    )?;
    state.control_groups.insert(label.0 as usize, GROUP);
    state.localized_controls.push((label, key));
    if generation {
        register_generation_control(state, label);
    } else {
        register_normal_control(state, label);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_editor(
    state: &mut SettingsState,
    key: &'static str,
    id: usize,
    text: &str,
    y: i32,
    height: i32,
    password: bool,
    generation: bool,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    create_editor_at(
        state, key, id, text, EDIT_LEFT, EDIT_WIDTH, y, height, password, generation, instance,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_editor_at(
    state: &mut SettingsState,
    key: &'static str,
    id: usize,
    text: &str,
    left: i32,
    width: i32,
    y: i32,
    height: i32,
    password: bool,
    generation: bool,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let mut style = WS_CHILD
        | WS_VISIBLE
        | WS_TABSTOP
        | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32 | WS_CLIPCHILDREN.0);
    if password {
        style |= WINDOW_STYLE(ES_PASSWORD as u32);
    }
    let editor = create_child(
        state,
        w!("EDIT"),
        text,
        style,
        left + 6,
        y + 5,
        width - 12,
        height - 10,
        id,
        instance,
    )?;
    apply_dark_theme(editor);
    if matches!(key, MATERIAL_KEY | USER_REQUIREMENTS_KEY) {
        // A native multiline EDIT otherwise accepts only 32,767 characters.
        // Leave request-size validation to the configured Rewrite API.
        unsafe {
            SendMessageW(
                editor,
                windows::Win32::UI::Controls::EM_SETLIMITTEXT,
                Some(WPARAM(i32::MAX as usize)),
                None,
            );
        }
    }
    let margin = platform::scale(7, state.dpi) as u32;
    unsafe {
        SendMessageW(
            editor,
            EM_SETMARGINS,
            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
            Some(LPARAM((margin | (margin << 16)) as isize)),
        );
    }
    dropdown_scrollbar::attach_edit(editor, state.dpi)?;
    state.input_frames.push(InputFrame {
        rect: RECT {
            left,
            top: y,
            right: left + width,
            bottom: y + height,
        },
        group: GROUP,
        control: editor,
    });
    state.controls.insert(key, editor);
    state.control_groups.insert(editor.0 as usize, GROUP);
    if generation {
        register_generation_control(state, editor);
    } else {
        register_normal_control(state, editor);
    }
    Ok(())
}

fn create_dynamic_form_controls(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    for (key, y, value_heading) in [
        ("advanced_audio_values", VALUES_LABEL_TOP, true),
        ("advanced_audio_secrets", SECRETS_LABEL_TOP, false),
    ] {
        let label = create_label(
            state,
            state.language.text(key),
            CONTENT_LEFT,
            y,
            EDIT_WIDTH,
            22,
            instance,
        )?;
        state.localized_controls.push((label, key));
        register_dynamic_control(state, label);
        if value_heading {
            state.advanced_audio.dynamic_values_heading = label;
        } else {
            state.advanced_audio.dynamic_secrets_heading = label;
        }
    }

    let (value_label, value_editor) =
        create_dynamic_input(state, ID_DYNAMIC_VALUE, VALUES_TOP, false, instance)?;
    let dynamic_boolean = create_dynamic_boolean(state, instance)?;
    let dynamic_select = create_dynamic_select(state, instance)?;
    let dynamic_multi_select = create_dynamic_multi_select(state, instance)?;
    let dynamic_json_object = create_dynamic_json_editor(state, ID_DYNAMIC_JSON_OBJECT, instance)?;
    let dynamic_json_array = create_dynamic_json_editor(state, ID_DYNAMIC_JSON_ARRAY, instance)?;
    let (secret_label, secret_editor) =
        create_dynamic_input(state, ID_DYNAMIC_SECRET, SECRETS_TOP, true, instance)?;
    state.advanced_audio.dynamic_value_label = value_label;
    state.advanced_audio.dynamic_value_editor = value_editor;
    state.advanced_audio.dynamic_boolean = dynamic_boolean;
    state.advanced_audio.dynamic_select = dynamic_select;
    state.advanced_audio.dynamic_multi_select = dynamic_multi_select;
    state.advanced_audio.dynamic_json_object = dynamic_json_object;
    state.advanced_audio.dynamic_json_array = dynamic_json_array;
    state.advanced_audio.dynamic_secret_label = secret_label;
    state.advanced_audio.dynamic_secret_editor = secret_editor;
    create_dynamic_label_tooltip(state, instance);

    for (key, id, x, label) in [
        (
            DYNAMIC_PREVIOUS_KEY,
            ID_DYNAMIC_PREVIOUS,
            CONTENT_LEFT,
            "advanced_audio_previous",
        ),
        (
            DYNAMIC_NEXT_KEY,
            ID_DYNAMIC_NEXT,
            CONTENT_LEFT + 72,
            "advanced_audio_next",
        ),
    ] {
        let button = create_child(
            state,
            w!("BUTTON"),
            state.language.text(label),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            x,
            VALIDATION_ACTION_TOP,
            64,
            FIELD_HEIGHT,
            id,
            instance,
        )?;
        state.controls.insert(key, button);
        state.localized_controls.push((button, label));
        register_dynamic_control(state, button);
        if id == ID_DYNAMIC_PREVIOUS {
            state.advanced_audio.dynamic_previous = button;
        } else {
            state.advanced_audio.dynamic_next = button;
        }
    }
    Ok(())
}

fn create_summary_controls(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let label = create_label(
        state,
        state.language.text("advanced_audio_summary"),
        CONTENT_LEFT,
        132,
        EDIT_WIDTH - SUMMARY_RESET_WIDTH - SUMMARY_RESET_GAP,
        22,
        instance,
    )?;
    state.control_groups.insert(label.0 as usize, GROUP);
    state
        .localized_controls
        .push((label, "advanced_audio_summary"));
    register_dynamic_control(state, label);

    let reset = create_child(
        state,
        w!("BUTTON"),
        state.language.text("advanced_audio_reset"),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        EDIT_LEFT + EDIT_WIDTH - SUMMARY_RESET_WIDTH,
        SUMMARY_RESET_TOP,
        SUMMARY_RESET_WIDTH,
        SUMMARY_RESET_HEIGHT,
        ID_RESET,
        instance,
    )?;
    state.controls.insert(RESET_KEY, reset);
    state
        .localized_controls
        .push((reset, "advanced_audio_reset"));
    register_dynamic_control(state, reset);

    let editor = create_child(
        state,
        w!("EDIT"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WINDOW_STYLE(
                (ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY) as u32 | WS_CLIPCHILDREN.0,
            ),
        EDIT_LEFT + 6,
        MATERIAL_TOP + 5,
        EDIT_WIDTH - 12,
        SUMMARY_HEIGHT - 10,
        ID_SUMMARY,
        instance,
    )?;
    apply_dark_theme(editor);
    let margin = platform::scale(7, state.dpi) as u32;
    unsafe {
        SendMessageW(
            editor,
            EM_SETMARGINS,
            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
            Some(LPARAM((margin | (margin << 16)) as isize)),
        );
    }
    dropdown_scrollbar::attach_edit(editor, state.dpi)?;
    state.input_frames.push(InputFrame {
        rect: RECT {
            left: EDIT_LEFT,
            top: MATERIAL_TOP,
            right: EDIT_LEFT + EDIT_WIDTH,
            bottom: MATERIAL_TOP + SUMMARY_HEIGHT,
        },
        group: GROUP,
        control: editor,
    });
    register_dynamic_control(state, editor);
    state.advanced_audio.summary_label = label;
    state.advanced_audio.summary_editor = editor;
    state.advanced_audio.summary_reset = reset;
    Ok(())
}

fn create_generation_status_control(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let editor = create_child(
        state,
        w!("EDIT"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WINDOW_STYLE(
                (ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY) as u32 | WS_CLIPCHILDREN.0,
            ),
        EDIT_LEFT + 6,
        GENERATION_STATUS_TOP + 5,
        EDIT_WIDTH - 12,
        GENERATION_STATUS_HEIGHT - 10,
        ID_GENERATION_STATUS,
        instance,
    )?;
    apply_dark_theme(editor);
    set_font(editor, state.small_font);
    let margin = platform::scale(7, state.dpi) as u32;
    unsafe {
        SendMessageW(
            editor,
            EM_SETMARGINS,
            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
            Some(LPARAM((margin | (margin << 16)) as isize)),
        );
    }
    dropdown_scrollbar::attach_edit(editor, state.dpi)?;
    state.input_frames.push(InputFrame {
        rect: RECT {
            left: EDIT_LEFT,
            top: GENERATION_STATUS_TOP,
            right: EDIT_LEFT + EDIT_WIDTH,
            bottom: GENERATION_STATUS_TOP + GENERATION_STATUS_HEIGHT,
        },
        group: GROUP,
        control: editor,
    });
    state.control_groups.insert(editor.0 as usize, GROUP);
    state.advanced_audio.generation_status = editor;
    Ok(())
}

fn create_dynamic_input(
    state: &mut SettingsState,
    id: usize,
    y: i32,
    password: bool,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(HWND, HWND), String> {
    let editor_left = CONTENT_LEFT + LABEL_WIDTH + 10;
    let editor_width = EDIT_WIDTH - LABEL_WIDTH - 10;
    let label = create_child(
        state,
        w!("STATIC"),
        "",
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | SS_CENTERIMAGE_STYLE | STATIC_STYLE_END_ELLIPSIS),
        CONTENT_LEFT,
        y,
        LABEL_WIDTH,
        FIELD_HEIGHT,
        0,
        instance,
    )?;
    register_dynamic_control(state, label);

    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    if password {
        style |= WINDOW_STYLE(ES_PASSWORD as u32);
    }
    let editor = create_child(
        state,
        w!("EDIT"),
        "",
        style,
        editor_left + 3,
        y + 7,
        editor_width - 6,
        20,
        id,
        instance,
    )?;
    apply_dark_theme(editor);
    let margin = platform::scale(7, state.dpi) as u32;
    unsafe {
        SendMessageW(
            editor,
            EM_SETMARGINS,
            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
            Some(LPARAM((margin | (margin << 16)) as isize)),
        );
    }
    state.input_frames.push(InputFrame {
        rect: RECT {
            left: editor_left,
            top: y,
            right: editor_left + editor_width,
            bottom: y + FIELD_HEIGHT,
        },
        group: GROUP,
        control: editor,
    });
    register_dynamic_control(state, editor);
    Ok((label, editor))
}

fn create_dynamic_label_tooltip(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) {
    let classes = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_WIN95_CLASSES,
    };
    unsafe {
        let _ = InitCommonControlsEx(&classes);
    }
    let Ok(tooltip) = (unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            TOOLTIPS_CLASSW,
            w!(""),
            WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX | TTS_USEVISUALSTYLE),
            0,
            0,
            0,
            0,
            Some(state.hwnd),
            None,
            Some(instance.into()),
            None,
        )
    }) else {
        // The labels still use a native ellipsis when the optional common
        // control cannot be created, so a visual enhancement cannot prevent
        // the settings window from opening.
        return;
    };

    apply_dark_theme(tooltip);
    unsafe {
        let _ = SetWindowPos(
            tooltip,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        let _ = SendMessageW(
            tooltip,
            TTM_SETMAXTIPWIDTH,
            None,
            Some(LPARAM(
                platform::scale(DYNAMIC_LABEL_TOOLTIP_MAX_WIDTH, state.dpi) as isize,
            )),
        );
        let _ = SendMessageW(
            tooltip,
            TTM_SETTIPBKCOLOR,
            Some(WPARAM(rgb(26, 35, 39).0 as usize)),
            None,
        );
        let _ = SendMessageW(
            tooltip,
            TTM_SETTIPTEXTCOLOR,
            Some(WPARAM(rgb(235, 242, 243).0 as usize)),
            None,
        );
        for tooltip_id in [
            ID_DYNAMIC_VALUE_LABEL_TOOLTIP,
            ID_DYNAMIC_SECRET_LABEL_TOOLTIP,
        ] {
            let tool = dynamic_label_tool_info(state.hwnd, tooltip_id, RECT::default());
            let _ = SendMessageW(
                tooltip,
                TTM_ADDTOOLW,
                None,
                Some(LPARAM((&raw const tool) as isize)),
            );
        }
    }
    state.advanced_audio.dynamic_label_tooltip = tooltip;
}

fn dynamic_label_tool_info(owner: HWND, tooltip_id: usize, rect: RECT) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_SUBCLASS,
        hwnd: owner,
        uId: tooltip_id,
        rect,
        lpszText: PWSTR::from_raw((-1_isize) as *mut u16),
        ..Default::default()
    }
}

fn dynamic_label_tool_rect(state: &SettingsState, top: i32, visible: bool) -> RECT {
    if !visible {
        return RECT::default();
    }
    let scale = |value| platform::scale(value, state.dpi);
    RECT {
        left: scale(CONTENT_LEFT),
        top: scale(top),
        right: scale(CONTENT_LEFT + LABEL_WIDTH),
        bottom: scale(top + FIELD_HEIGHT),
    }
}

fn sync_dynamic_label_tooltip_rects(
    state: &SettingsState,
    value_visible: bool,
    secret_visible: bool,
) {
    let tooltip = state.advanced_audio.dynamic_label_tooltip;
    if tooltip.is_invalid() {
        return;
    }
    unsafe {
        for (tooltip_id, top, visible) in [
            (ID_DYNAMIC_VALUE_LABEL_TOOLTIP, VALUES_TOP, value_visible),
            (ID_DYNAMIC_SECRET_LABEL_TOOLTIP, SECRETS_TOP, secret_visible),
        ] {
            let tool = dynamic_label_tool_info(
                state.hwnd,
                tooltip_id,
                dynamic_label_tool_rect(state, top, visible),
            );
            let _ = SendMessageW(
                tooltip,
                TTM_NEWTOOLRECTW,
                None,
                Some(LPARAM((&raw const tool) as isize)),
            );
        }
    }
}

fn pop_dynamic_label_tooltip(state: &SettingsState) {
    let tooltip = state.advanced_audio.dynamic_label_tooltip;
    if !tooltip.is_invalid() {
        unsafe {
            let _ = SendMessageW(tooltip, TTM_POP, None, None);
        }
    }
}

fn resize_dynamic_label_tooltip(state: &SettingsState) {
    let tooltip = state.advanced_audio.dynamic_label_tooltip;
    if tooltip.is_invalid() {
        return;
    }
    unsafe {
        let _ = SendMessageW(
            tooltip,
            TTM_SETMAXTIPWIDTH,
            None,
            Some(LPARAM(
                platform::scale(DYNAMIC_LABEL_TOOLTIP_MAX_WIDTH, state.dpi) as isize,
            )),
        );
    }
    sync_dynamic_label_tooltip_rects(
        state,
        unsafe { IsWindowVisible(state.advanced_audio.dynamic_value_label).as_bool() },
        unsafe { IsWindowVisible(state.advanced_audio.dynamic_secret_label).as_bool() },
    );
}

fn dynamic_input_bounds() -> (i32, i32) {
    let left = CONTENT_LEFT + LABEL_WIDTH + 10;
    let width = EDIT_WIDTH - LABEL_WIDTH - 10;
    (left, width)
}

fn create_dynamic_boolean(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<HWND, String> {
    let (left, _) = dynamic_input_bounds();
    let checkbox = create_child(
        state,
        w!("BUTTON"),
        "",
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        left,
        VALUES_TOP,
        FIELD_HEIGHT,
        FIELD_HEIGHT,
        ID_DYNAMIC_BOOLEAN,
        instance,
    )?;
    state.boolean_values.insert(DYNAMIC_BOOLEAN_KEY, false);
    state
        .boolean_ids
        .insert(ID_DYNAMIC_BOOLEAN, DYNAMIC_BOOLEAN_KEY);
    register_dynamic_control(state, checkbox);
    Ok(checkbox)
}

fn create_dynamic_select(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<HWND, String> {
    let (left, width) = dynamic_input_bounds();
    let select = create_child(
        state,
        w!("BUTTON"),
        "",
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        left,
        VALUES_TOP,
        width,
        FIELD_HEIGHT,
        ID_DYNAMIC_SELECT,
        instance,
    )?;
    let panel = dropdown::create_panel(state, instance)?;
    let list = create_child(
        state,
        w!("LISTBOX"),
        "",
        WINDOW_STYLE(
            WS_CHILD.0
                | WS_TABSTOP.0
                | (LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT) as u32,
        ),
        0,
        0,
        width,
        dropdown::ROW_HEIGHT,
        ID_DYNAMIC_SELECT_LIST,
        instance,
    )?;
    apply_dark_theme(list);
    dropdown::track_hover(list, true)?;
    unsafe {
        SetParent(list, Some(panel)).map_err(|error| error.to_string())?;
        if !SetWindowSubclass(
            select,
            Some(dynamic_select_proc),
            ID_DYNAMIC_SELECT,
            state.hwnd.0 as usize,
        )
        .as_bool()
            || !SetWindowSubclass(
                list,
                Some(dynamic_select_proc),
                ID_DYNAMIC_SELECT_LIST,
                state.hwnd.0 as usize,
            )
            .as_bool()
        {
            return Err("Unable to initialize dynamic select dropdown".into());
        }
    }
    let scrollbar = dropdown_scrollbar::create(state, panel, list, instance)?;
    register_dynamic_control(state, select);
    state.advanced_audio.dynamic_select_panel = panel;
    state.advanced_audio.dynamic_select_list = list;
    state.advanced_audio.dynamic_select_scrollbar = scrollbar;
    Ok(select)
}

fn create_dynamic_multi_select(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<HWND, String> {
    let (left, width) = dynamic_input_bounds();
    let button = create_child(
        state,
        w!("BUTTON"),
        "",
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        left,
        VALUES_TOP,
        width,
        FIELD_HEIGHT,
        ID_DYNAMIC_MULTI_SELECT,
        instance,
    )?;
    let panel = dropdown::create_panel(state, instance)?;
    let list = create_child(
        state,
        w!("LISTBOX"),
        "",
        WINDOW_STYLE(
            WS_CHILD.0
                | WS_TABSTOP.0
                | (LBS_HASSTRINGS
                    | LBS_MULTIPLESEL
                    | LBS_NOINTEGRALHEIGHT
                    | LBS_NOTIFY
                    | LBS_OWNERDRAWFIXED) as u32,
        ),
        0,
        0,
        width,
        MULTI_SELECT_ROW_HEIGHT,
        ID_DYNAMIC_MULTI_SELECT_LIST,
        instance,
    )?;
    apply_dark_theme(list);
    dropdown::track_hover(list, true)?;
    unsafe {
        SetParent(list, Some(panel)).map_err(|error| error.to_string())?;
        if !SetWindowSubclass(
            button,
            Some(dynamic_multi_select_proc),
            ID_DYNAMIC_MULTI_SELECT,
            state.hwnd.0 as usize,
        )
        .as_bool()
            || !SetWindowSubclass(
                list,
                Some(dynamic_multi_select_proc),
                ID_DYNAMIC_MULTI_SELECT_LIST,
                state.hwnd.0 as usize,
            )
            .as_bool()
        {
            return Err("Unable to initialize dynamic multi-select dropdown".into());
        }
        SendMessageW(
            list,
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(
                platform::scale(MULTI_SELECT_ROW_HEIGHT, state.dpi) as isize
            )),
        );
    }
    let scrollbar = dropdown_scrollbar::create(state, panel, list, instance)?;
    register_dynamic_control(state, button);
    state.advanced_audio.dynamic_multi_select_panel = panel;
    state.advanced_audio.dynamic_multi_select_list = list;
    state.advanced_audio.dynamic_multi_select_scrollbar = scrollbar;
    Ok(button)
}

fn create_dynamic_json_editor(
    state: &mut SettingsState,
    id: usize,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<HWND, String> {
    let (left, width) = dynamic_input_bounds();
    let editor = create_child(
        state,
        w!("EDIT"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WINDOW_STYLE(
                (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32 | WS_CLIPCHILDREN.0,
            ),
        left + 3,
        VALUES_TOP + 3,
        width - 6,
        JSON_EDITOR_HEIGHT - 6,
        id,
        instance,
    )?;
    apply_dark_theme(editor);
    // JSON configuration can legitimately be longer than the native EDIT
    // control's default multiline limit. Core remains responsible for
    // validating the JSON shape and request size.
    unsafe {
        SendMessageW(
            editor,
            windows::Win32::UI::Controls::EM_SETLIMITTEXT,
            Some(WPARAM(i32::MAX as usize)),
            None,
        );
    }
    let margin = platform::scale(7, state.dpi) as u32;
    unsafe {
        SendMessageW(
            editor,
            EM_SETMARGINS,
            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
            Some(LPARAM((margin | (margin << 16)) as isize)),
        );
    }
    dropdown_scrollbar::attach_edit(editor, state.dpi)?;
    state.input_frames.push(InputFrame {
        rect: RECT {
            left,
            top: VALUES_TOP,
            right: left + width,
            bottom: VALUES_TOP + JSON_EDITOR_HEIGHT,
        },
        group: GROUP,
        control: editor,
    });
    register_dynamic_control(state, editor);
    Ok(editor)
}

fn refresh_dynamic_form(state: &mut SettingsState) {
    let form = state
        .controls
        .get(WORKFLOW_KEY)
        .and_then(|control| parse_workflow(*control, "").ok().flatten())
        .filter(|workflow| validate_workflow(workflow).is_ok())
        .map(|workflow| DynamicForm::from_workflow(&workflow));

    let changed = match (&state.advanced_audio.dynamic_form, &form) {
        (Some(current), Some(next)) => {
            current.parameters != next.parameters || current.secrets != next.secrets
        }
        (None, None) => false,
        _ => true,
    };

    if changed {
        if let Some(form) = form {
            let parameter_ids: BTreeSet<_> = form
                .parameters
                .iter()
                .map(|definition| definition.id.clone())
                .collect();
            let secret_ids: BTreeSet<_> = form
                .secrets
                .iter()
                .map(|definition| definition.id.clone())
                .collect();
            state
                .advanced_audio
                .config
                .values
                .retain(|id, _| parameter_ids.contains(id));
            state
                .advanced_audio
                .config
                .secrets
                .retain(|id, _| secret_ids.contains(id));
            state.advanced_audio.dynamic_form = Some(form);
        } else {
            state.advanced_audio.dynamic_form = None;
        }
    }

    normalize_dynamic_page(state);
    sync_dynamic_form_page(state);
}

fn normalize_dynamic_page(state: &mut SettingsState) {
    let values = state.advanced_audio.config.values.clone();
    if let Some(form) = &mut state.advanced_audio.dynamic_form {
        form.normalize_page(&values);
    }
}

fn sync_dynamic_form_page(state: &mut SettingsState) {
    // A queued listbox notification belongs to the page that was visible when
    // it was posted. Rebinding the shared listbox invalidates that update.
    state.advanced_audio.dynamic_multi_select_sync_pending = None;
    close_dynamic_select(state);
    pop_dynamic_label_tooltip(state);
    let (value, secret) = current_dynamic_inputs(state);
    let secret_label = state.advanced_audio.dynamic_secret_label;
    let secret_editor = state.advanced_audio.dynamic_secret_editor;

    state.advanced_audio.updating_dynamic_controls = true;
    sync_dynamic_value_controls(state, value.as_ref());
    set_dynamic_text_input(secret_label, secret_editor, secret.as_ref(), state);
    state.advanced_audio.updating_dynamic_controls = false;
    sync_summary_control(state);
}

fn sync_summary_control(state: &SettingsState) {
    let text = match &state.advanced_audio.validation {
        ValidationState::Valid(summary) => summary_rows(summary, state.language).join("\r\n"),
        ValidationState::NotValidated | ValidationState::Invalid(_) => state
            .controls
            .get(WORKFLOW_KEY)
            .and_then(|control| parse_workflow(*control, "").ok().flatten())
            .filter(|workflow| validate_workflow(workflow).is_ok())
            .map(|workflow| {
                summary_rows(
                    &workflow_summary(&workflow, &state.advanced_audio.config.remote_audio),
                    state.language,
                )
                .join("\r\n")
            })
            .unwrap_or_default(),
    };
    let text = wide(&text);
    unsafe {
        let _ = SetWindowTextW(state.advanced_audio.summary_editor, PCWSTR(text.as_ptr()));
    }
}

fn current_dynamic_inputs(state: &SettingsState) -> (Option<DynamicInput>, Option<DynamicInput>) {
    let page = &state.advanced_audio;
    let Some(form) = &page.dynamic_form else {
        return (None, None);
    };
    let Some(slot) = form.current_page(&page.config.values) else {
        return (None, None);
    };
    let value = slot
        .parameter_index
        .and_then(|index| form.parameters.get(index))
        .cloned();
    let secret = slot
        .secret_index
        .and_then(|index| form.secrets.get(index))
        .cloned();
    (value, secret)
}

fn dynamic_input_value(
    values: &std::collections::BTreeMap<String, String>,
    definition: &DynamicInput,
) -> String {
    values
        .get(&definition.id)
        .cloned()
        .or_else(|| definition.default.clone())
        .unwrap_or_default()
}

fn set_dynamic_text_input(
    label: HWND,
    editor: HWND,
    definition: Option<&DynamicInput>,
    state: &SettingsState,
) {
    let (label_text, value_text) = definition.map_or_else(
        || (String::new(), String::new()),
        |definition| {
            (
                definition.label.clone(),
                dynamic_input_value(&state.advanced_audio.config.secrets, definition),
            )
        },
    );
    unsafe {
        if read_text(label) != label_text {
            let label_text = wide(&label_text);
            let _ = SetWindowTextW(label, PCWSTR(label_text.as_ptr()));
        }
        if read_text(editor) != value_text {
            let value_text = wide(&value_text);
            let _ = SetWindowTextW(editor, PCWSTR(value_text.as_ptr()));
        }
    }
}

fn sync_dynamic_value_controls(state: &mut SettingsState, definition: Option<&DynamicInput>) {
    let label = state.advanced_audio.dynamic_value_label;
    let editor = state.advanced_audio.dynamic_value_editor;
    let boolean = state.advanced_audio.dynamic_boolean;
    let json_object = state.advanced_audio.dynamic_json_object;
    let json_array = state.advanced_audio.dynamic_json_array;
    let (label_text, value) = definition.map_or_else(
        || (String::new(), String::new()),
        |definition| {
            (
                definition.label.clone(),
                dynamic_input_value(&state.advanced_audio.config.values, definition),
            )
        },
    );
    let label_text_wide = wide(&label_text);
    unsafe {
        if read_text(label) != label_text {
            let _ = SetWindowTextW(label, PCWSTR(label_text_wide.as_ptr()));
        }
    }
    match definition.map(|definition| definition.parameter_type) {
        Some(ParameterType::Text | ParameterType::Integer | ParameterType::Number) => {
            set_dynamic_editor_value(editor, &value);
        }
        Some(ParameterType::Boolean) => {
            state
                .boolean_values
                .insert(DYNAMIC_BOOLEAN_KEY, value == "true");
            unsafe {
                let _ = InvalidateRect(Some(boolean), None, true);
            }
        }
        Some(ParameterType::Select) => {
            sync_dynamic_select_options(state, definition.expect("select definition"), &value);
        }
        Some(ParameterType::MultiSelect) => {
            sync_dynamic_multi_select_options(
                state,
                definition.expect("multi-select definition"),
                &value,
            );
        }
        Some(ParameterType::JsonObject) => set_dynamic_editor_value(json_object, &value),
        Some(ParameterType::JsonArray) => set_dynamic_editor_value(json_array, &value),
        None => {}
    }
}

fn set_dynamic_editor_value(editor: HWND, value: &str) {
    if read_text(editor) != value {
        let value = wide(value);
        unsafe {
            let _ = SetWindowTextW(editor, PCWSTR(value.as_ptr()));
        }
    }
}

fn dynamic_option_label(option: &ParameterOption) -> &str {
    if option.label.is_empty() {
        &option.value
    } else {
        &option.label
    }
}

fn sync_dynamic_select_options(state: &SettingsState, definition: &DynamicInput, value: &str) {
    let button = state.advanced_audio.dynamic_select;
    let list = state.advanced_audio.dynamic_select_list;
    let selected = definition
        .options
        .iter()
        .position(|option| option.value == value);
    let label = selected
        .and_then(|index| definition.options.get(index))
        .map(dynamic_option_label)
        .unwrap_or(value);
    unsafe {
        if read_text(button) != label {
            let _ = SetWindowTextW(button, PCWSTR(wide(label).as_ptr()));
        }
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for option in &definition.options {
            let text = wide(dynamic_option_label(option));
            SendMessageW(
                list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
        }
        SendMessageW(
            list,
            LB_SETCURSEL,
            Some(WPARAM(selected.unwrap_or(usize::MAX))),
            None,
        );
        SendMessageW(
            list,
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(
                platform::scale(dropdown::ROW_HEIGHT, state.dpi) as isize
            )),
        );
    }
    if dynamic_select_is_open(state) {
        let _ = layout_dynamic_select_popup(state);
    }
}

fn sync_dynamic_multi_select_options(
    state: &SettingsState,
    definition: &DynamicInput,
    value: &str,
) {
    let list = state.advanced_audio.dynamic_multi_select_list;
    let selected = serde_json::from_str::<Vec<String>>(value).unwrap_or_default();
    unsafe {
        SendMessageW(list, LB_RESETCONTENT, None, None);
        SendMessageW(
            list,
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(
                platform::scale(MULTI_SELECT_ROW_HEIGHT, state.dpi) as isize
            )),
        );
        for (index, option) in definition.options.iter().enumerate() {
            let text = wide(dynamic_option_label(option));
            SendMessageW(
                list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
            if selected.iter().any(|item| item == &option.value) {
                SendMessageW(
                    list,
                    LB_SETSEL,
                    Some(WPARAM(1)),
                    Some(LPARAM(index as isize)),
                );
            }
        }
    }
    set_dynamic_multi_select_button_text(state, definition, &selected);
    if dynamic_multi_select_is_open(state) {
        let _ = layout_dynamic_multi_select_popup(state);
    }
}

fn set_dynamic_multi_select_button_text(
    state: &SettingsState,
    definition: &DynamicInput,
    selected: &[String],
) {
    let text = selected
        .iter()
        .map(|value| {
            definition
                .options
                .iter()
                .find(|option| option.value == *value)
                .map(dynamic_option_label)
                .unwrap_or(value)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let button = state.advanced_audio.dynamic_multi_select;
    unsafe {
        if read_text(button) != text {
            let _ = SetWindowTextW(button, PCWSTR(wide(&text).as_ptr()));
        }
        let _ = InvalidateRect(Some(button), None, true);
    }
}

fn sync_dynamic_text_input(state: &mut SettingsState, id: usize) {
    if state.advanced_audio.updating_dynamic_controls {
        return;
    }
    let (definition, editor, secret) = {
        let page = &state.advanced_audio;
        let Some(form) = &page.dynamic_form else {
            return;
        };
        let Some(slot) = form.current_page(&page.config.values) else {
            return;
        };
        let (definition, editor, secret) = match id {
            ID_DYNAMIC_VALUE => (
                slot.parameter_index
                    .and_then(|index| form.parameters.get(index)),
                page.dynamic_value_editor,
                false,
            ),
            ID_DYNAMIC_JSON_OBJECT => (
                slot.parameter_index
                    .and_then(|index| form.parameters.get(index)),
                page.dynamic_json_object,
                false,
            ),
            ID_DYNAMIC_JSON_ARRAY => (
                slot.parameter_index
                    .and_then(|index| form.parameters.get(index)),
                page.dynamic_json_array,
                false,
            ),
            ID_DYNAMIC_SECRET => (
                slot.secret_index.and_then(|index| form.secrets.get(index)),
                page.dynamic_secret_editor,
                true,
            ),
            _ => return,
        };
        let Some(definition) = definition else {
            return;
        };
        let accepts_input = matches!(
            (id, definition.parameter_type),
            (
                ID_DYNAMIC_VALUE,
                ParameterType::Text | ParameterType::Integer | ParameterType::Number
            ) | (ID_DYNAMIC_JSON_OBJECT, ParameterType::JsonObject)
                | (ID_DYNAMIC_JSON_ARRAY, ParameterType::JsonArray)
                | (ID_DYNAMIC_SECRET, _)
        );
        if !accepts_input {
            return;
        }
        (definition.clone(), editor, secret)
    };
    let value = read_text(editor);
    let target = if secret {
        &mut state.advanced_audio.config.secrets
    } else {
        &mut state.advanced_audio.config.values
    };
    store_dynamic_input_value(target, &definition.id, value);
}

fn store_dynamic_input_value(
    values: &mut std::collections::BTreeMap<String, String>,
    id: &str,
    value: String,
) {
    if value.is_empty() {
        values.remove(id);
    } else {
        values.insert(id.into(), value);
    }
}

fn sync_dynamic_boolean(state: &mut SettingsState) {
    let Some(definition) = current_dynamic_inputs(state).0 else {
        return;
    };
    if definition.parameter_type != ParameterType::Boolean {
        return;
    }
    let value = !state
        .boolean_values
        .get(DYNAMIC_BOOLEAN_KEY)
        .copied()
        .unwrap_or(false);
    state.boolean_values.insert(DYNAMIC_BOOLEAN_KEY, value);
    state
        .advanced_audio
        .config
        .values
        .insert(definition.id, if value { "true" } else { "false" }.into());
}

fn sync_dynamic_multi_select(state: &mut SettingsState) {
    let Some(definition) = current_dynamic_inputs(state).0 else {
        return;
    };
    if definition.parameter_type != ParameterType::MultiSelect {
        return;
    }
    let list = state.advanced_audio.dynamic_multi_select_list;
    let selected = definition
        .options
        .iter()
        .enumerate()
        .filter_map(|(index, option)| {
            (unsafe { SendMessageW(list, LB_GETSEL, Some(WPARAM(index)), None).0 > 0 })
                .then(|| option.value.clone())
        })
        .collect::<Vec<_>>();
    let default = definition
        .default
        .as_deref()
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default();
    if selected.is_empty() && default.is_empty() {
        state.advanced_audio.config.values.remove(&definition.id);
    } else {
        state.advanced_audio.config.values.insert(
            definition.id.clone(),
            serde_json::to_string(&selected).expect("string arrays serialize"),
        );
    }
    set_dynamic_multi_select_button_text(state, &definition, &selected);
}

fn dynamic_multi_select_is_open(state: &SettingsState) -> bool {
    let page = &state.advanced_audio;
    !page.dynamic_multi_select_panel.is_invalid()
        && unsafe { IsWindowVisible(page.dynamic_multi_select_panel).as_bool() }
}

fn layout_dynamic_multi_select_popup(state: &SettingsState) -> bool {
    let Some(definition) = current_dynamic_inputs(state).0 else {
        return false;
    };
    if definition.parameter_type != ParameterType::MultiSelect {
        return false;
    }
    let page = &state.advanced_audio;
    let panel = page.dynamic_multi_select_panel;
    let list = page.dynamic_multi_select_list;
    let scrollbar = page.dynamic_multi_select_scrollbar;
    if panel.is_invalid() || list.is_invalid() || scrollbar.is_invalid() {
        return false;
    }
    let rows = definition.options.len().clamp(1, MULTI_SELECT_VISIBLE_ROWS) as i32;
    let list_height = platform::scale(rows * MULTI_SELECT_ROW_HEIGHT, state.dpi);
    let width = dropdown::position(
        panel,
        page.dynamic_multi_select,
        rows * MULTI_SELECT_ROW_HEIGHT,
        state.dpi,
    );
    let list_width = dropdown_scrollbar::position(
        scrollbar,
        width,
        list_height,
        state.dpi,
        definition.options.len() > MULTI_SELECT_VISIBLE_ROWS,
    );
    unsafe {
        let _ = SetWindowPos(
            list,
            Some(HWND_TOP),
            platform::scale(dropdown::PADDING, state.dpi),
            platform::scale(dropdown::PADDING, state.dpi),
            list_width,
            list_height,
            SWP_NOACTIVATE,
        );
        let _ = ShowWindow(list, SW_SHOW);
    }
    true
}

fn dynamic_select_is_open(state: &SettingsState) -> bool {
    let panel = state.advanced_audio.dynamic_select_panel;
    !panel.is_invalid() && unsafe { IsWindowVisible(panel).as_bool() }
}

fn layout_dynamic_select_popup(state: &SettingsState) -> bool {
    let Some(definition) = current_dynamic_inputs(state).0 else {
        return false;
    };
    if definition.parameter_type != ParameterType::Select {
        return false;
    }
    let page = &state.advanced_audio;
    let panel = page.dynamic_select_panel;
    let list = page.dynamic_select_list;
    let scrollbar = page.dynamic_select_scrollbar;
    if panel.is_invalid() || list.is_invalid() || scrollbar.is_invalid() {
        return false;
    }
    let rows = definition.options.len().clamp(1, DYNAMIC_SELECT_MAX_ROWS) as i32;
    let list_height = platform::scale(rows * dropdown::ROW_HEIGHT, state.dpi);
    let width = dropdown::position(
        panel,
        page.dynamic_select,
        rows * dropdown::ROW_HEIGHT,
        state.dpi,
    );
    let list_width = dropdown_scrollbar::position(
        scrollbar,
        width,
        list_height,
        state.dpi,
        definition.options.len() > DYNAMIC_SELECT_MAX_ROWS,
    );
    unsafe {
        let _ = SetWindowPos(
            list,
            Some(HWND_TOP),
            platform::scale(dropdown::PADDING, state.dpi),
            platform::scale(dropdown::PADDING, state.dpi),
            list_width,
            list_height,
            SWP_NOACTIVATE,
        );
        let _ = ShowWindow(list, SW_SHOW);
    }
    true
}

fn open_dynamic_select(state: &mut SettingsState) {
    if !layout_dynamic_select_popup(state) {
        return;
    }
    close_dynamic_multi_select(state);
    let panel = state.advanced_audio.dynamic_select_panel;
    let list = state.advanced_audio.dynamic_select_list;
    let button = state.advanced_audio.dynamic_select;
    unsafe {
        let _ = ShowWindow(panel, SW_SHOW);
        let _ = SetFocus(Some(list));
        let _ = InvalidateRect(Some(button), None, true);
    }
}

fn close_dynamic_select_popup(state: &mut SettingsState) {
    let panel = state.advanced_audio.dynamic_select_panel;
    let button = state.advanced_audio.dynamic_select;
    if panel.is_invalid() {
        return;
    }
    unsafe {
        let _ = ShowWindow(panel, SW_HIDE);
        let _ = InvalidateRect(Some(button), None, true);
    }
}

fn open_dynamic_multi_select(state: &mut SettingsState) {
    if !layout_dynamic_multi_select_popup(state) {
        return;
    }
    close_dynamic_select_popup(state);
    let panel = state.advanced_audio.dynamic_multi_select_panel;
    let list = state.advanced_audio.dynamic_multi_select_list;
    let button = state.advanced_audio.dynamic_multi_select;
    unsafe {
        let _ = ShowWindow(panel, SW_SHOW);
        let _ = SetFocus(Some(list));
        let _ = InvalidateRect(Some(button), None, true);
    }
}

fn close_dynamic_multi_select(state: &mut SettingsState) {
    let panel = state.advanced_audio.dynamic_multi_select_panel;
    let button = state.advanced_audio.dynamic_multi_select;
    if panel.is_invalid() {
        return;
    }
    unsafe {
        let _ = ShowWindow(panel, SW_HIDE);
        let _ = InvalidateRect(Some(button), None, true);
    }
}

pub(super) fn close_dynamic_select(state: &mut SettingsState) {
    close_dynamic_select_popup(state);
    close_dynamic_multi_select(state);
}

fn toggle_dynamic_select(state: &mut SettingsState) {
    if dynamic_select_is_open(state) {
        close_dynamic_select_popup(state);
    } else {
        open_dynamic_select(state);
    }
}

fn toggle_dynamic_multi_select(state: &mut SettingsState) {
    if dynamic_multi_select_is_open(state) {
        close_dynamic_multi_select(state);
    } else {
        open_dynamic_multi_select(state);
    }
}

fn choose_dynamic_select_option(state: &mut SettingsState) {
    let Some(definition) = current_dynamic_inputs(state).0 else {
        return;
    };
    if definition.parameter_type != ParameterType::Select {
        return;
    }
    let selected = unsafe {
        SendMessageW(
            state.advanced_audio.dynamic_select_list,
            LB_GETCURSEL,
            None,
            None,
        )
        .0
    };
    let Some(option) = usize::try_from(selected)
        .ok()
        .and_then(|index| definition.options.get(index))
    else {
        return;
    };
    let refresh_dynamic_layout = current_dynamic_value_affects_visibility(state);
    state
        .advanced_audio
        .config
        .values
        .insert(definition.id.clone(), option.value.clone());
    unsafe {
        let _ = SetWindowTextW(
            state.advanced_audio.dynamic_select,
            PCWSTR(wide(dynamic_option_label(option)).as_ptr()),
        );
    }
    close_dynamic_select_popup(state);
    dynamic_input_changed(state, refresh_dynamic_layout);
}

unsafe extern "system" fn dynamic_select_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    parent: usize,
) -> LRESULT {
    let parent = HWND(parent as *mut std::ffi::c_void);
    let list = unsafe { GetDlgCtrlID(hwnd) as usize == ID_DYNAMIC_SELECT_LIST };
    let post = |action: usize, detail: LPARAM| unsafe {
        let _ = PostMessageW(Some(parent), DYNAMIC_SELECT_ACTION, WPARAM(action), detail);
    };
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        if list && [VK_RETURN.0, VK_SPACE.0, VK_ESCAPE.0, VK_TAB.0].contains(&key) {
            post(
                if key == VK_TAB.0 {
                    DYNAMIC_SELECT_TAB
                } else if key == VK_ESCAPE.0 {
                    DYNAMIC_SELECT_CLOSE
                } else {
                    DYNAMIC_SELECT_PICK
                },
                LPARAM((unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0) as isize),
            );
            return LRESULT(0);
        }
        if !list && [VK_DOWN.0, VK_RETURN.0, VK_SPACE.0].contains(&key) {
            post(DYNAMIC_SELECT_TOGGLE, LPARAM(0));
            return LRESULT(0);
        }
        if !list && key == VK_TAB.0 {
            post(
                DYNAMIC_SELECT_TAB,
                LPARAM((unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0) as isize),
            );
            return LRESULT(0);
        }
    }
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if list && message == WM_LBUTTONUP {
        let hit = unsafe { SendMessageW(hwnd, LB_ITEMFROMPOINT, None, Some(lparam)) }.0 as usize;
        if hit >> 16 == 0 {
            post(DYNAMIC_SELECT_PICK, LPARAM(0));
        }
    } else if message == WM_KILLFOCUS {
        post(DYNAMIC_SELECT_CLOSE_IF_OUTSIDE, LPARAM(wparam.0 as isize));
    }
    result
}

unsafe extern "system" fn dynamic_multi_select_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    parent: usize,
) -> LRESULT {
    let parent = HWND(parent as *mut std::ffi::c_void);
    let list = unsafe { GetDlgCtrlID(hwnd) as usize == ID_DYNAMIC_MULTI_SELECT_LIST };
    let post = |action: usize, detail: LPARAM| unsafe {
        let _ = PostMessageW(
            Some(parent),
            DYNAMIC_MULTI_SELECT_ACTION,
            WPARAM(action),
            detail,
        );
    };
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        if list && [VK_RETURN.0, VK_ESCAPE.0, VK_TAB.0].contains(&key) {
            post(
                if key == VK_TAB.0 {
                    DYNAMIC_SELECT_TAB
                } else {
                    DYNAMIC_SELECT_CLOSE
                },
                LPARAM((unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0) as isize),
            );
            return LRESULT(0);
        }
        if !list && [VK_DOWN.0, VK_RETURN.0, VK_SPACE.0].contains(&key) {
            post(DYNAMIC_SELECT_TOGGLE, LPARAM(0));
            return LRESULT(0);
        }
        if !list && key == VK_TAB.0 {
            post(
                DYNAMIC_SELECT_TAB,
                LPARAM((unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0) as isize),
            );
            return LRESULT(0);
        }
    }
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if list && message == WM_KILLFOCUS {
        post(DYNAMIC_SELECT_CLOSE_IF_OUTSIDE, LPARAM(wparam.0 as isize));
    }
    result
}

pub(super) fn handle_message(
    state: &mut SettingsState,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> bool {
    if message == WM_NOTIFY {
        return handle_dynamic_label_tooltip_notification(state, lparam);
    }
    if message == DYNAMIC_MULTI_SELECT_SYNC {
        if !state
            .advanced_audio
            .dynamic_multi_select_sync_pending
            .as_ref()
            .is_some_and(|pending| pending.token == wparam.0)
        {
            return true;
        }
        let parameter_id = state
            .advanced_audio
            .dynamic_multi_select_sync_pending
            .take()
            .expect("matching pending multi-select sync")
            .parameter_id;
        let active = current_dynamic_inputs(state).0.is_some_and(|definition| {
            definition.parameter_type == ParameterType::MultiSelect && definition.id == parameter_id
        });
        if active
            && !state.saving
            && enabled(state)
            && !is_generating(state)
            && !state.advanced_audio.updating_dynamic_controls
        {
            // LBN_SELCHANGE is delivered while the native LISTBOX is still
            // processing its selection. Read and persist its state only after
            // that notification has returned to avoid re-entering it through
            // LB_GETSEL and repaint messages.
            sync_dynamic_multi_select(state);
            dynamic_input_changed(state, false);
        }
        return true;
    }
    if message == DYNAMIC_MULTI_SELECT_ACTION {
        return handle_dynamic_multi_select_action(state, wparam, lparam);
    }
    if message != DYNAMIC_SELECT_ACTION {
        return false;
    }
    let action = wparam.0;
    let select_button = state.advanced_audio.dynamic_select;
    let select_list = state.advanced_audio.dynamic_select_list;
    let select_scrollbar = state.advanced_audio.dynamic_select_scrollbar;
    let select_active = current_dynamic_inputs(state)
        .0
        .is_some_and(|definition| definition.parameter_type == ParameterType::Select);
    match action {
        DYNAMIC_SELECT_TOGGLE if !state.saving && enabled(state) && !is_generating(state) => {
            if select_active {
                toggle_dynamic_select(state);
            }
        }
        DYNAMIC_SELECT_PICK if !state.saving && enabled(state) && !is_generating(state) => {
            if select_active {
                choose_dynamic_select_option(state);
                unsafe {
                    if IsWindowVisible(select_button).as_bool() {
                        let _ = SetFocus(Some(select_button));
                    }
                }
            }
        }
        DYNAMIC_SELECT_CLOSE => {
            close_dynamic_select_popup(state);
            unsafe {
                let _ = SetFocus(Some(select_button));
            }
        }
        DYNAMIC_SELECT_CLOSE_IF_OUTSIDE => {
            let focus = unsafe { GetFocus() };
            if ![select_button, select_list, select_scrollbar].contains(&focus) {
                close_dynamic_select_popup(state);
            }
        }
        DYNAMIC_SELECT_TAB => {
            close_dynamic_select_popup(state);
            unsafe {
                if let Ok(next) = GetNextDlgTabItem(state.hwnd, Some(select_button), lparam.0 != 0)
                {
                    let _ = SetFocus(Some(next));
                }
            }
        }
        _ => {}
    }
    true
}

fn handle_dynamic_label_tooltip_notification(state: &mut SettingsState, lparam: LPARAM) -> bool {
    if lparam.0 == 0 || state.advanced_audio.dynamic_label_tooltip.is_invalid() {
        return false;
    }
    let header = unsafe { &*(lparam.0 as *const NMHDR) };
    if header.hwndFrom != state.advanced_audio.dynamic_label_tooltip
        || header.code != TTN_GETDISPINFOW
    {
        return false;
    }
    let notification = unsafe { &mut *(lparam.0 as *mut NMTTDISPINFOW) };
    match notification.hdr.idFrom {
        ID_DYNAMIC_VALUE_LABEL_TOOLTIP => {
            state.advanced_audio.dynamic_value_label_tooltip_text =
                wide(&read_text(state.advanced_audio.dynamic_value_label));
            notification.lpszText = PWSTR(
                state
                    .advanced_audio
                    .dynamic_value_label_tooltip_text
                    .as_mut_ptr(),
            );
        }
        ID_DYNAMIC_SECRET_LABEL_TOOLTIP => {
            state.advanced_audio.dynamic_secret_label_tooltip_text =
                wide(&read_text(state.advanced_audio.dynamic_secret_label));
            notification.lpszText = PWSTR(
                state
                    .advanced_audio
                    .dynamic_secret_label_tooltip_text
                    .as_mut_ptr(),
            );
        }
        _ => return false,
    }
    true
}

pub(super) fn destroy(state: &mut SettingsState) {
    let tooltip = std::mem::take(&mut state.advanced_audio.dynamic_label_tooltip);
    if !tooltip.is_invalid() {
        unsafe {
            let _ = DestroyWindow(tooltip);
        }
    }
}

fn handle_dynamic_multi_select_action(
    state: &mut SettingsState,
    wparam: WPARAM,
    lparam: LPARAM,
) -> bool {
    let action = wparam.0;
    let button = state.advanced_audio.dynamic_multi_select;
    let list = state.advanced_audio.dynamic_multi_select_list;
    let scrollbar = state.advanced_audio.dynamic_multi_select_scrollbar;
    let active = current_dynamic_inputs(state)
        .0
        .is_some_and(|definition| definition.parameter_type == ParameterType::MultiSelect);
    match action {
        DYNAMIC_SELECT_TOGGLE if !state.saving && enabled(state) && !is_generating(state) => {
            if active {
                toggle_dynamic_multi_select(state);
            }
        }
        DYNAMIC_SELECT_CLOSE => {
            close_dynamic_multi_select(state);
            unsafe {
                let _ = SetFocus(Some(button));
            }
        }
        DYNAMIC_SELECT_CLOSE_IF_OUTSIDE => {
            let focus = unsafe { GetFocus() };
            if ![button, list, scrollbar].contains(&focus) {
                close_dynamic_multi_select(state);
            }
        }
        DYNAMIC_SELECT_TAB => {
            close_dynamic_multi_select(state);
            unsafe {
                if let Ok(next) = GetNextDlgTabItem(state.hwnd, Some(button), lparam.0 != 0) {
                    let _ = SetFocus(Some(next));
                }
            }
        }
        _ => {}
    }
    true
}

fn dynamic_input_changed(state: &mut SettingsState, refresh_dynamic_layout: bool) {
    state.advanced_audio.validation = ValidationState::NotValidated;
    state.advanced_audio.test_requested = false;
    if refresh_dynamic_layout {
        normalize_dynamic_page(state);
        sync_dynamic_form_page(state);
        update_controls(state);
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), None, true);
        }
    } else {
        // A normal edit already repaints its own native control. Repaint only
        // the validation/test feedback that its changed value invalidates,
        // rather than flashing the whole settings page for each character.
        let scale = |value| platform::scale(value, state.dpi);
        let feedback = RECT {
            left: scale(CONTENT_LEFT),
            top: scale(VALIDATION_ACTION_TOP),
            right: scale(CONTENT_LEFT + EDIT_WIDTH),
            bottom: scale(WINDOW_HEIGHT - FOOTER_HEIGHT),
        };
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), Some(&feedback), true);
        }
    }
}

fn current_dynamic_value_affects_visibility(state: &SettingsState) -> bool {
    let Some(definition) = current_dynamic_inputs(state).0 else {
        return false;
    };
    state
        .advanced_audio
        .dynamic_form
        .as_ref()
        .is_some_and(|form| form.has_visibility_dependents(&definition.id))
}

fn change_dynamic_page(state: &mut SettingsState, offset: isize) {
    let values = state.advanced_audio.config.values.clone();
    let Some(form) = &mut state.advanced_audio.dynamic_form else {
        return;
    };
    let page_count = form.pages(&values).len();
    if page_count == 0 {
        return;
    }
    form.page = if offset.is_negative() {
        form.page.saturating_sub(offset.unsigned_abs())
    } else {
        form.page
            .saturating_add(offset as usize)
            .min(page_count - 1)
    };
    sync_dynamic_form_page(state);
    sync_visibility(state);
    update_controls(state);
    // Dynamic input frames are painted by the parent window. A page change can
    // hide a text or secret editor, so repaint the parent once to erase its
    // former frame before the next page is shown.
    unsafe {
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }
}

fn create_remote_editor_controls(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    create_remote_viewport(state, instance)?;
    create_remote_label(
        state,
        "advanced_audio_remote_hosting",
        CONTENT_LEFT,
        94,
        EDIT_WIDTH,
        FIELD_HEIGHT,
        RemoteControlGroup::Common,
        instance,
    )?;
    create_remote_label(
        state,
        "advanced_audio_remote_provider",
        CONTENT_LEFT,
        132,
        EDIT_WIDTH,
        22,
        RemoteControlGroup::Common,
        instance,
    )?;

    for (key, id, x, width, label) in [
        (
            REMOTE_NONE_KEY,
            ID_REMOTE_NONE,
            CONTENT_LEFT,
            70,
            "advanced_audio_remote_none",
        ),
        (
            REMOTE_WEBDAV_KEY,
            ID_REMOTE_WEBDAV,
            CONTENT_LEFT + 78,
            132,
            "advanced_audio_remote_webdav",
        ),
        (
            REMOTE_S3_KEY,
            ID_REMOTE_S3,
            CONTENT_LEFT + 218,
            146,
            "advanced_audio_remote_s3",
        ),
        (
            REMOTE_OSS_KEY,
            ID_REMOTE_OSS,
            CONTENT_LEFT + 372,
            164,
            "advanced_audio_remote_oss",
        ),
    ] {
        let button = create_child(
            state,
            w!("BUTTON"),
            state.language.text(label),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            x,
            158,
            width,
            FIELD_HEIGHT,
            id,
            instance,
        )?;
        state.controls.insert(key, button);
        state.control_groups.insert(button.0 as usize, GROUP);
        state.localized_controls.push((button, label));
        register_remote_control(state, button, RemoteControlGroup::Common);
    }

    let mut field_id = ID_REMOTE_FIELD_BASE;
    for (key, label, password) in REMOTE_WEBDAV_FIELDS {
        create_remote_text_field(
            state,
            key,
            label,
            REMOTE_FIELD_TOP + (field_id - ID_REMOTE_FIELD_BASE) as i32 * REMOTE_FIELD_STEP,
            password,
            RemoteControlGroup::Webdav,
            field_id,
            instance,
        )?;
        field_id += 1;
    }
    for (key, label, password) in REMOTE_S3_FIELDS {
        create_remote_text_field(
            state,
            key,
            label,
            REMOTE_FIELD_TOP
                + (field_id - ID_REMOTE_FIELD_BASE - REMOTE_WEBDAV_FIELDS.len()) as i32
                    * REMOTE_FIELD_STEP,
            password,
            RemoteControlGroup::S3,
            field_id,
            instance,
        )?;
        field_id += 1;
    }
    for (key, label, password) in REMOTE_OSS_FIELDS {
        create_remote_text_field(
            state,
            key,
            label,
            REMOTE_FIELD_TOP
                + (field_id
                    - ID_REMOTE_FIELD_BASE
                    - REMOTE_WEBDAV_FIELDS.len()
                    - REMOTE_S3_FIELDS.len()) as i32
                    * REMOTE_FIELD_STEP,
            password,
            RemoteControlGroup::Oss,
            field_id,
            instance,
        )?;
        field_id += 1;
    }

    create_remote_checkbox(
        state,
        REMOTE_PRESIGNED_KEY,
        ID_REMOTE_PRESIGNED,
        "advanced_audio_remote_presigned",
        REMOTE_FIELD_TOP + REMOTE_S3_FIELDS.len() as i32 * REMOTE_FIELD_STEP,
        RemoteControlGroup::Presigned,
        instance,
    )?;
    create_remote_checkbox(
        state,
        REMOTE_DELETE_AFTER_KEY,
        ID_REMOTE_DELETE_AFTER,
        "advanced_audio_remote_delete_after",
        REMOTE_FIELD_TOP + (REMOTE_S3_FIELDS.len() as i32 + 1) * REMOTE_FIELD_STEP,
        RemoteControlGroup::Cleanup,
        instance,
    )?;

    for (key, id, x, width, label) in [
        (
            REMOTE_BACK_KEY,
            ID_REMOTE_BACK,
            CONTENT_LEFT,
            132,
            "advanced_audio_remote_back",
        ),
        (
            REMOTE_APPLY_KEY,
            ID_REMOTE_APPLY,
            CONTENT_LEFT + 144,
            132,
            "advanced_audio_remote_apply",
        ),
    ] {
        let button = create_child(
            state,
            w!("BUTTON"),
            state.language.text(label),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            x,
            REMOTE_ACTION_TOP,
            width,
            FIELD_HEIGHT,
            id,
            instance,
        )?;
        state.controls.insert(key, button);
        state.control_groups.insert(button.0 as usize, GROUP);
        state.localized_controls.push((button, label));
        register_remote_control(state, button, RemoteControlGroup::Common);
    }

    sync_remote_controls_from_draft(state);
    initialize_remote_viewport(state)?;
    layout_remote_viewport(state);
    Ok(())
}

fn create_remote_viewport(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let rect = RECT {
        left: CONTENT_LEFT,
        top: REMOTE_FIELD_TOP,
        right: WINDOW_WIDTH,
        bottom: REMOTE_FIELD_TOP + REMOTE_VIEWPORT_HEIGHT,
    };
    let viewport = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            w!("STATIC"),
            w!(""),
            WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            platform::scale(rect.left, state.dpi),
            platform::scale(rect.top, state.dpi),
            platform::scale(rect.right - rect.left, state.dpi),
            platform::scale(rect.bottom - rect.top, state.dpi),
            Some(state.hwnd),
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|error| error.to_string())?;
    set_font(viewport, state.font);
    state.layouts.borrow_mut().push((viewport, rect));
    state.control_groups.insert(viewport.0 as usize, GROUP);
    state.advanced_audio.remote_viewport = viewport;
    Ok(())
}

fn initialize_remote_viewport(state: &mut SettingsState) -> Result<(), String> {
    let viewport = state.advanced_audio.remote_viewport;
    let content_controls = remote_content_controls(state);
    let layouts = {
        let layouts = state.layouts.borrow();
        layouts
            .iter()
            .filter(|(control, _)| content_controls.contains(control))
            .map(|(control, rect)| RemoteLayout {
                control: *control,
                rect: *rect,
            })
            .collect()
    };

    for control in &content_controls {
        unsafe {
            SetParent(*control, Some(viewport)).map_err(|error| error.to_string())?;
        }
    }
    for control in remote_all_controls(state) {
        if !unsafe {
            SetWindowSubclass(
                control,
                Some(remote_control_proc),
                REMOTE_CONTROL_SUBCLASS_ID,
                viewport.0 as usize,
            )
        }
        .as_bool()
        {
            return Err("Unable to initialize remote hosting scrolling".into());
        }
    }
    if !unsafe {
        SetWindowSubclass(
            viewport,
            Some(remote_viewport_proc),
            REMOTE_VIEWPORT_SUBCLASS_ID,
            state.hwnd.0 as usize,
        )
    }
    .as_bool()
    {
        return Err("Unable to initialize remote hosting viewport".into());
    }

    let scrollbar = dropdown_scrollbar::attach_viewport(viewport, state.dpi)?;
    state.advanced_audio.remote_layouts = layouts;
    state.advanced_audio.remote_scrollbar = scrollbar;
    Ok(())
}

fn remote_content_controls(state: &SettingsState) -> Vec<HWND> {
    state
        .advanced_audio
        .remote_webdav_controls
        .iter()
        .chain(state.advanced_audio.remote_s3_controls.iter())
        .chain(state.advanced_audio.remote_oss_controls.iter())
        .chain(state.advanced_audio.remote_presigned_controls.iter())
        .chain(state.advanced_audio.remote_cleanup_controls.iter())
        .copied()
        .collect()
}

fn remote_all_controls(state: &SettingsState) -> Vec<HWND> {
    state
        .advanced_audio
        .remote_common_controls
        .iter()
        .chain(remote_content_controls(state).iter())
        .copied()
        .collect()
}

fn remote_toggle_positions(provider: RemoteProvider) -> (Option<i32>, Option<i32>) {
    let field_bottom = |count: usize| REMOTE_FIELD_TOP + count as i32 * REMOTE_FIELD_STEP;
    match provider {
        RemoteProvider::None => (None, None),
        RemoteProvider::Webdav => (None, Some(field_bottom(REMOTE_WEBDAV_FIELDS.len()))),
        RemoteProvider::S3Compatible => {
            let presigned = field_bottom(REMOTE_S3_FIELDS.len());
            (Some(presigned), Some(presigned + REMOTE_FIELD_STEP))
        }
        RemoteProvider::AliyunOss => {
            let presigned = field_bottom(REMOTE_OSS_FIELDS.len());
            (Some(presigned), Some(presigned + REMOTE_FIELD_STEP))
        }
    }
}

fn move_remote_controls(layouts: &mut [RemoteLayout], controls: &[HWND], top: i32) {
    let Some(current_top) = layouts
        .iter()
        .filter(|layout| controls.contains(&layout.control))
        .map(|layout| layout.rect.top)
        .min()
    else {
        return;
    };
    let offset = top - current_top;
    for layout in layouts
        .iter_mut()
        .filter(|layout| controls.contains(&layout.control))
    {
        layout.rect.top += offset;
        layout.rect.bottom += offset;
    }
}

fn update_remote_toggle_layouts(state: &mut SettingsState) {
    let provider = remote_provider(&state.advanced_audio.remote_draft);
    let (presigned_top, cleanup_top) = remote_toggle_positions(provider);
    let presigned_controls = state.advanced_audio.remote_presigned_controls.clone();
    let cleanup_controls = state.advanced_audio.remote_cleanup_controls.clone();
    let layouts = &mut state.advanced_audio.remote_layouts;
    if let Some(top) = presigned_top {
        move_remote_controls(layouts, &presigned_controls, top);
    }
    if let Some(top) = cleanup_top {
        move_remote_controls(layouts, &cleanup_controls, top);
    }
}

fn remote_content_extent(state: &SettingsState) -> i32 {
    let (_, cleanup_top) =
        remote_toggle_positions(remote_provider(&state.advanced_audio.remote_draft));
    let bottom = cleanup_top
        .map(|top| top + FIELD_HEIGHT)
        .unwrap_or(REMOTE_FIELD_TOP);
    platform::scale(
        bottom - REMOTE_FIELD_TOP + REMOTE_CONTENT_BOTTOM_PADDING,
        state.dpi,
    )
}

fn remote_viewport_rect(state: &SettingsState, rect: RECT) -> RECT {
    let rect = scaled_rect(rect, state.dpi);
    RECT {
        left: rect.left - platform::scale(CONTENT_LEFT, state.dpi),
        top: rect.top
            - platform::scale(REMOTE_FIELD_TOP, state.dpi)
            - state.advanced_audio.remote_offset,
        right: rect.right - platform::scale(CONTENT_LEFT, state.dpi),
        bottom: rect.bottom
            - platform::scale(REMOTE_FIELD_TOP, state.dpi)
            - state.advanced_audio.remote_offset,
    }
}

fn layout_remote_viewport(state: &mut SettingsState) {
    let viewport = state.advanced_audio.remote_viewport;
    let scrollbar = state.advanced_audio.remote_scrollbar;
    if viewport.is_invalid() || scrollbar.is_invalid() {
        return;
    }

    update_remote_toggle_layouts(state);
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(viewport, &mut client);
    }
    let max_offset = (remote_content_extent(state) - client.bottom).max(0);
    state.advanced_audio.remote_offset = state.advanced_audio.remote_offset.clamp(0, max_offset);
    let layouts = state.advanced_audio.remote_layouts.clone();
    unsafe {
        for layout in layouts {
            let rect = remote_viewport_rect(state, layout.rect);
            let _ = SetWindowPos(
                layout.control,
                None,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        let right = client.right - platform::scale(10, state.dpi);
        let left = right - dropdown_scrollbar::thickness(state.dpi);
        dropdown_scrollbar::position_rect(
            scrollbar,
            RECT {
                left,
                top: platform::scale(6, state.dpi),
                right,
                bottom: client.bottom - platform::scale(6, state.dpi),
            },
            state.dpi,
        );
        let _ = ShowWindow(scrollbar, if max_offset > 0 { SW_SHOW } else { SW_HIDE });
        let _ = InvalidateRect(Some(viewport), None, true);
    }
}

fn reset_remote_viewport(state: &mut SettingsState) {
    state.advanced_audio.remote_offset = 0;
    layout_remote_viewport(state);
}

fn reveal_remote_control(state: &mut SettingsState, control: HWND) {
    let Some(layout) = state
        .advanced_audio
        .remote_layouts
        .iter()
        .find(|layout| layout.control == control)
        .copied()
    else {
        return;
    };
    let viewport = state.advanced_audio.remote_viewport;
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(viewport, &mut client);
    }
    let rect = remote_viewport_rect(state, layout.rect);
    let margin = platform::scale(8, state.dpi);
    let target = if rect.top < margin {
        state.advanced_audio.remote_offset + rect.top - margin
    } else if rect.bottom > client.bottom - margin {
        state.advanced_audio.remote_offset + rect.bottom - client.bottom + margin
    } else {
        state.advanced_audio.remote_offset
    };
    let max_offset = (remote_content_extent(state) - client.bottom).max(0);
    let target = target.clamp(0, max_offset);
    if target != state.advanced_audio.remote_offset {
        state.advanced_audio.remote_offset = target;
        layout_remote_viewport(state);
    }
}

pub(super) fn resize(state: &mut SettingsState) {
    if !state.advanced_audio.generation_status.is_invalid() {
        set_font(state.advanced_audio.generation_status, state.small_font);
    }
    if !state.advanced_audio.dynamic_multi_select_list.is_invalid() {
        unsafe {
            SendMessageW(
                state.advanced_audio.dynamic_multi_select_list,
                LB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(
                    platform::scale(MULTI_SELECT_ROW_HEIGHT, state.dpi) as isize
                )),
            );
        }
    }
    sync_dynamic_form_page(state);
    resize_dynamic_label_tooltip(state);
    layout_remote_viewport(state);
}

unsafe extern "system" fn remote_viewport_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    let root = HWND(data as *mut std::ffi::c_void);
    let pointer = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
            root,
            windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
        )
    } as *mut SettingsState;
    if pointer.is_null() {
        if message == WM_NCDESTROY {
            unsafe {
                let _ = RemoveWindowSubclass(
                    hwnd,
                    Some(remote_viewport_proc),
                    REMOTE_VIEWPORT_SUBCLASS_ID,
                );
            }
        }
        return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &mut *pointer };
    match message {
        dropdown_scrollbar::VIEWPORT_GET_EXTENT => LRESULT(remote_content_extent(state) as isize),
        dropdown_scrollbar::VIEWPORT_GET_OFFSET => {
            LRESULT(state.advanced_audio.remote_offset as isize)
        }
        dropdown_scrollbar::VIEWPORT_SET_OFFSET => {
            if state.advanced_audio.remote_offset != wparam.0 as i32 {
                state.advanced_audio.remote_offset = wparam.0 as i32;
                layout_remote_viewport(state);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => unsafe {
            SendMessageW(
                state.advanced_audio.remote_scrollbar,
                message,
                Some(wparam),
                Some(lparam),
            )
        },
        REMOTE_REVEAL => {
            reveal_remote_control(state, HWND(lparam.0 as *mut std::ffi::c_void));
            LRESULT(0)
        }
        WM_COMMAND | WM_DRAWITEM | WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => unsafe {
            SendMessageW(state.hwnd, message, Some(wparam), Some(lparam))
        },
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
            let mut client = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut client);
                FillRect(hdc, &client, state.background_brush);
                let focused = GetFocus();
                for frame in state.input_frames.iter().filter(|frame| {
                    GetParent(frame.control).ok() == Some(hwnd)
                        && IsWindowVisible(frame.control).as_bool()
                }) {
                    rounded_box(
                        hdc,
                        remote_viewport_rect(state, frame.rect),
                        rgb(26, 35, 39),
                        if focused == frame.control {
                            rgb(92, 192, 176)
                        } else {
                            rgb(54, 68, 74)
                        },
                        platform::scale(7, state.dpi),
                    );
                }
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => unsafe {
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(remote_viewport_proc),
                REMOTE_VIEWPORT_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

unsafe extern "system" fn remote_control_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    let viewport = HWND(data as *mut std::ffi::c_void);
    if message == WM_MOUSEWHEEL {
        return unsafe { SendMessageW(viewport, message, Some(wparam), Some(lparam)) };
    }
    if message == WM_SETFOCUS {
        unsafe {
            let _ = SendMessageW(viewport, REMOTE_REVEAL, None, Some(LPARAM(hwnd.0 as isize)));
        }
    }
    if message == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(remote_control_proc), id);
        }
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[allow(clippy::too_many_arguments)]
fn create_remote_text_field(
    state: &mut SettingsState,
    key: &'static str,
    label: &'static str,
    y: i32,
    password: bool,
    group: RemoteControlGroup,
    id: usize,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    const REMOTE_LABEL_WIDTH: i32 = 148;
    let editor_left = CONTENT_LEFT + REMOTE_LABEL_WIDTH + 10;
    let editor_width = EDIT_WIDTH - REMOTE_LABEL_WIDTH - 10;
    create_remote_label(
        state,
        label,
        CONTENT_LEFT,
        y,
        REMOTE_LABEL_WIDTH,
        FIELD_HEIGHT,
        group,
        instance,
    )?;
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    if password {
        style |= WINDOW_STYLE(ES_PASSWORD as u32);
    }
    let editor = create_child(
        state,
        w!("EDIT"),
        "",
        style,
        editor_left + 3,
        y + 7,
        editor_width - 6,
        20,
        id,
        instance,
    )?;
    apply_dark_theme(editor);
    let margin = platform::scale(7, state.dpi) as u32;
    unsafe {
        SendMessageW(
            editor,
            EM_SETMARGINS,
            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
            Some(LPARAM((margin | (margin << 16)) as isize)),
        );
    }
    state.input_frames.push(InputFrame {
        rect: RECT {
            left: editor_left,
            top: y,
            right: editor_left + editor_width,
            bottom: y + FIELD_HEIGHT,
        },
        group: GROUP,
        control: editor,
    });
    state.controls.insert(key, editor);
    state.control_groups.insert(editor.0 as usize, GROUP);
    register_remote_control(state, editor, group);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_remote_checkbox(
    state: &mut SettingsState,
    key: &'static str,
    id: usize,
    label: &'static str,
    y: i32,
    group: RemoteControlGroup,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    create_remote_label(
        state,
        label,
        CONTENT_LEFT,
        y,
        EDIT_WIDTH - 36,
        FIELD_HEIGHT,
        group,
        instance,
    )?;
    let checkbox = create_child(
        state,
        w!("BUTTON"),
        "",
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        CONTENT_LEFT + EDIT_WIDTH - 26,
        y + 5,
        24,
        24,
        id,
        instance,
    )?;
    state.controls.insert(key, checkbox);
    state.control_groups.insert(checkbox.0 as usize, GROUP);
    state.boolean_values.insert(key, false);
    state.boolean_ids.insert(id, key);
    register_remote_control(state, checkbox, group);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_remote_label(
    state: &mut SettingsState,
    key: &'static str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    group: RemoteControlGroup,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let label = create_label(
        state,
        state.language.text(key),
        x,
        y,
        width,
        height,
        instance,
    )?;
    state.control_groups.insert(label.0 as usize, GROUP);
    state.localized_controls.push((label, key));
    register_remote_control(state, label, group);
    Ok(())
}

pub(super) fn command(state: &mut SettingsState, id: usize, notification: u32) -> bool {
    if matches!(
        id,
        ID_DYNAMIC_VALUE | ID_DYNAMIC_SECRET | ID_DYNAMIC_JSON_OBJECT | ID_DYNAMIC_JSON_ARRAY
    ) && notification == EN_CHANGE
    {
        if !state.advanced_audio.updating_dynamic_controls {
            sync_dynamic_text_input(state, id);
            dynamic_input_changed(state, false);
        }
        return true;
    }
    if id == ID_DYNAMIC_BOOLEAN && notification == BN_CLICKED {
        if !state.saving && !is_generating(state) && !state.advanced_audio.updating_dynamic_controls
        {
            let refresh_dynamic_layout = current_dynamic_value_affects_visibility(state);
            sync_dynamic_boolean(state);
            dynamic_input_changed(state, refresh_dynamic_layout);
        }
        return true;
    }
    if id == ID_DYNAMIC_SELECT && notification == BN_CLICKED {
        if !state.saving && !is_generating(state) && !state.advanced_audio.updating_dynamic_controls
        {
            toggle_dynamic_select(state);
        }
        return true;
    }
    if id == ID_DYNAMIC_SELECT_LIST && notification == LBN_SELCHANGE {
        return true;
    }
    if id == ID_DYNAMIC_MULTI_SELECT && notification == BN_CLICKED {
        if !state.saving && !is_generating(state) && !state.advanced_audio.updating_dynamic_controls
        {
            toggle_dynamic_multi_select(state);
        }
        return true;
    }
    if id == ID_DYNAMIC_MULTI_SELECT_LIST && notification == LBN_SELCHANGE {
        if !state.saving
            && !is_generating(state)
            && !state.advanced_audio.updating_dynamic_controls
            && state
                .advanced_audio
                .dynamic_multi_select_sync_pending
                .is_none()
        {
            let parameter_id = current_dynamic_inputs(state)
                .0
                .filter(|definition| definition.parameter_type == ParameterType::MultiSelect)
                .map(|definition| definition.id);
            if let Some(parameter_id) = parameter_id {
                let token = state
                    .advanced_audio
                    .dynamic_multi_select_sync_token
                    .wrapping_add(1);
                state.advanced_audio.dynamic_multi_select_sync_token = token;
                state.advanced_audio.dynamic_multi_select_sync_pending =
                    Some(DynamicMultiSelectSync {
                        token,
                        parameter_id,
                    });
                let posted = unsafe {
                    PostMessageW(
                        Some(state.hwnd),
                        DYNAMIC_MULTI_SELECT_SYNC,
                        WPARAM(token),
                        LPARAM(0),
                    )
                };
                if posted.is_err()
                    && state
                        .advanced_audio
                        .dynamic_multi_select_sync_pending
                        .as_ref()
                        .is_some_and(|pending| pending.token == token)
                {
                    state.advanced_audio.dynamic_multi_select_sync_pending = None;
                }
            }
        }
        return true;
    }
    if id == ID_DYNAMIC_PREVIOUS && notification == BN_CLICKED {
        if !state.saving && !is_generating(state) {
            change_dynamic_page(state, -1);
        }
        return true;
    }
    if id == ID_DYNAMIC_NEXT && notification == BN_CLICKED {
        if !state.saving && !is_generating(state) {
            change_dynamic_page(state, 1);
        }
        return true;
    }
    if is_remote_field(id) && notification == EN_CHANGE {
        state.advanced_audio.test_requested = false;
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), None, true);
        }
        return true;
    }
    if matches!(id, ID_MATERIAL | ID_USER_REQUIREMENTS)
        && notification == EN_CHANGE
        && !is_generating(state)
    {
        state.advanced_audio.generation = GenerationState::Idle;
        sync_visibility(state);
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), None, true);
        }
        return true;
    }
    if id == ID_WORKFLOW && notification == EN_CHANGE {
        state.advanced_audio.validation = ValidationState::NotValidated;
        state.advanced_audio.test_requested = false;
        if !is_generating(state) {
            state.advanced_audio.generation = GenerationState::Idle;
        }
        refresh_dynamic_form(state);
        update_controls(state);
        sync_visibility(state);
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), None, true);
        }
        return true;
    }
    if id == ID_WORKFLOW && notification == EN_KILLFOCUS {
        format_json_input(state.controls[WORKFLOW_KEY]);
        refresh_dynamic_form(state);
        sync_visibility(state);
        return true;
    }
    if id == ID_ENABLE && notification == BN_CLICKED {
        if state.saving || is_generating(state) {
            return true;
        }
        let enabled = !enabled(state);
        state.boolean_values.insert(ENABLE_KEY, enabled);
        state.advanced_audio.validation = ValidationState::NotValidated;
        state.advanced_audio.generation = GenerationState::Idle;
        state.advanced_audio.test_requested = false;
        update_controls(state);
        sync_visibility(state);
        unsafe {
            let _ = InvalidateRect(Some(state.controls[ENABLE_KEY]), None, true);
            let _ = InvalidateRect(Some(state.hwnd), None, true);
        }
        return true;
    }
    if id == ID_REMOTE_OPEN && notification == BN_CLICKED {
        if !state.saving
            && enabled(state)
            && !is_generating(state)
            && workflow_requires_remote_draft(state)
        {
            state.advanced_audio.remote_draft = state.advanced_audio.config.remote_audio.clone();
            state.advanced_audio.remote_open = true;
            sync_remote_controls_from_draft(state);
            reset_remote_viewport(state);
            sync_visibility(state);
            update_controls(state);
            unsafe {
                let _ = InvalidateRect(Some(state.hwnd), None, true);
            }
        }
        return true;
    }
    if id == ID_REMOTE_BACK && notification == BN_CLICKED {
        if !state.saving && state.advanced_audio.remote_open {
            state.advanced_audio.remote_draft = state.advanced_audio.config.remote_audio.clone();
            state.advanced_audio.remote_open = false;
            sync_remote_controls_from_draft(state);
            sync_visibility(state);
            update_controls(state);
            unsafe {
                let _ = InvalidateRect(Some(state.hwnd), None, true);
            }
        }
        return true;
    }
    if id == ID_REMOTE_APPLY && notification == BN_CLICKED {
        if !state.saving && state.advanced_audio.remote_open {
            sync_remote_draft_from_controls(state);
            state.advanced_audio.config.remote_audio = state.advanced_audio.remote_draft.clone();
            state.advanced_audio.remote_open = false;
            state.advanced_audio.test_requested = false;
            validate_workflow_draft(state);
            sync_visibility(state);
            update_controls(state);
            unsafe {
                let _ = InvalidateRect(Some(state.hwnd), None, true);
            }
        }
        return true;
    }
    if let Some(provider) = remote_provider_for_button(id)
        && notification == BN_CLICKED
    {
        if !state.saving && state.advanced_audio.remote_open {
            sync_remote_draft_from_controls(state);
            set_remote_provider(state, provider);
            sync_remote_controls_from_draft(state);
            reset_remote_viewport(state);
            sync_visibility(state);
            update_controls(state);
            unsafe {
                let _ = InvalidateRect(Some(state.hwnd), None, true);
            }
        }
        return true;
    }
    if matches!(id, ID_REMOTE_PRESIGNED | ID_REMOTE_DELETE_AFTER) && notification == BN_CLICKED {
        if !state.saving && state.advanced_audio.remote_open {
            sync_remote_draft_from_controls(state);
            toggle_remote_boolean(state, id);
            sync_remote_controls_from_draft(state);
            layout_remote_viewport(state);
            unsafe {
                let _ = InvalidateRect(Some(state.hwnd), None, true);
            }
        }
        return true;
    }
    if id == ID_GENERATE
        && notification == BN_CLICKED
        && !state.saving
        && enabled(state)
        && !is_generating(state)
    {
        start_generation(state);
        return true;
    }
    if id == ID_CANCEL_GENERATION && notification == BN_CLICKED && is_generating(state) {
        cancel_generation(state);
        state.advanced_audio.generation = GenerationState::Canceled;
        update_controls(state);
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), None, true);
        }
        return true;
    }
    if id == ID_RESET
        && notification == BN_CLICKED
        && !state.saving
        && enabled(state)
        && !is_generating(state)
        && !state.advanced_audio.remote_open
        && !matches!(
            state.connectivity_status,
            super::ConnectivityStatus::Testing
        )
    {
        reset_workflow_draft(state);
        return true;
    }
    if !matches!(id, ID_VALIDATE | ID_TEST) || notification != BN_CLICKED || state.saving {
        return false;
    }

    state.advanced_audio.generation = GenerationState::Idle;
    validate_workflow_draft(state);
    if id == ID_TEST
        && !matches!(
            state.connectivity_status,
            super::ConnectivityStatus::Testing
        )
        && let ValidationState::Valid(summary) = &state.advanced_audio.validation
    {
        let preview = summary.test_preview.clone();
        if confirm_workflow_test(state, &preview) {
            state.advanced_audio.test_requested = true;
            // Use the same Core AudioApiClient path as normal transcription.
            // This runs the configured workflow rather than a GUI-only request.
            super::start_connectivity(state, false);
            update_controls(state);
        }
    }
    sync_visibility(state);
    unsafe {
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }
    true
}

fn is_remote_field(id: usize) -> bool {
    (ID_REMOTE_FIELD_BASE
        ..ID_REMOTE_FIELD_BASE
            + REMOTE_WEBDAV_FIELDS.len()
            + REMOTE_S3_FIELDS.len()
            + REMOTE_OSS_FIELDS.len())
        .contains(&id)
}

fn remote_provider_for_button(id: usize) -> Option<RemoteProvider> {
    match id {
        ID_REMOTE_NONE => Some(RemoteProvider::None),
        ID_REMOTE_WEBDAV => Some(RemoteProvider::Webdav),
        ID_REMOTE_S3 => Some(RemoteProvider::S3Compatible),
        ID_REMOTE_OSS => Some(RemoteProvider::AliyunOss),
        _ => None,
    }
}

fn validate_workflow_draft(state: &mut SettingsState) {
    let remote_audio = state.advanced_audio.config.remote_audio.clone();
    let validation = parse_workflow(
        state.controls[WORKFLOW_KEY],
        state.language.text("advanced_audio_workflow_json"),
    )
    .and_then(|workflow| {
        if enabled(state) && workflow.is_none() {
            return Err("Workflow JSON is required when Advanced Audio API is enabled.".into());
        }
        let Some(workflow) = workflow else {
            return Ok(None);
        };
        validate_workflow(&workflow).map_err(|errors| {
            errors
                .errors()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        validate_remote_draft(&workflow, &remote_audio)?;
        Ok(Some(workflow_summary(&workflow, &remote_audio)))
    });
    state.advanced_audio.validation = match validation {
        Ok(Some(summary)) => ValidationState::Valid(summary),
        Ok(None) => ValidationState::NotValidated,
        Err(error) => ValidationState::Invalid(error),
    };
    refresh_dynamic_form(state);
}

fn validate_remote_draft(
    workflow: &AdvancedAudioWorkflow,
    remote_audio: &RemoteAudioConfig,
) -> Result<(), String> {
    // Dynamic values and secrets may still be incomplete while the user is
    // editing a workflow. Validate only the remote-hosting portion here;
    // Core validates every typed value before a workflow can run or save.
    validate_remote_audio_config(workflow, remote_audio).map_err(|errors| {
        errors
            .errors()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn workflow_requires_remote_draft(state: &SettingsState) -> bool {
    state
        .controls
        .get(WORKFLOW_KEY)
        .and_then(|control| {
            serde_json::from_str::<AdvancedAudioWorkflow>(&read_text(*control)).ok()
        })
        .is_some_and(|workflow| workflow_requires_remote(&workflow))
}

fn workflow_requires_remote(workflow: &AdvancedAudioWorkflow) -> bool {
    matches!(
        &workflow.audio.delivery,
        AudioDelivery::PublicHttpsUrl | AudioDelivery::CloudUri
    )
}

fn remote_provider(config: &RemoteAudioConfig) -> RemoteProvider {
    match config {
        RemoteAudioConfig::None => RemoteProvider::None,
        RemoteAudioConfig::Webdav(_) => RemoteProvider::Webdav,
        RemoteAudioConfig::S3Compatible(_) => RemoteProvider::S3Compatible,
        RemoteAudioConfig::AliyunOss(_) => RemoteProvider::AliyunOss,
    }
}

fn set_remote_provider(state: &mut SettingsState, provider: RemoteProvider) {
    if remote_provider(&state.advanced_audio.remote_draft) == provider {
        return;
    }
    state.advanced_audio.remote_draft = match provider {
        RemoteProvider::None => RemoteAudioConfig::None,
        RemoteProvider::Webdav => RemoteAudioConfig::Webdav(Default::default()),
        RemoteProvider::S3Compatible => RemoteAudioConfig::S3Compatible(Default::default()),
        RemoteProvider::AliyunOss => RemoteAudioConfig::AliyunOss(Default::default()),
    };
}

fn sync_remote_draft_from_controls(state: &mut SettingsState) {
    let webdav = remote_field_values(state, &REMOTE_WEBDAV_FIELDS);
    let s3 = remote_field_values(state, &REMOTE_S3_FIELDS);
    let oss = remote_field_values(state, &REMOTE_OSS_FIELDS);
    let presigned = remote_boolean(state, REMOTE_PRESIGNED_KEY);
    let delete_after = remote_boolean(state, REMOTE_DELETE_AFTER_KEY);
    match &mut state.advanced_audio.remote_draft {
        RemoteAudioConfig::None => {}
        RemoteAudioConfig::Webdav(config) => {
            config.upload_base_url = webdav[0].clone();
            config.username = webdav[1].clone();
            config.password = webdav[2].clone();
            config.remote_path_prefix = webdav[3].clone();
            config.public_download_base_url = webdav[4].clone();
            config.delete_after_recognition = delete_after;
        }
        RemoteAudioConfig::S3Compatible(config) => {
            config.endpoint = s3[0].clone();
            config.region = s3[1].clone();
            config.bucket = s3[2].clone();
            config.access_key = s3[3].clone();
            config.secret_key = s3[4].clone();
            config.prefix = s3[5].clone();
            config.public_url_base = nonempty_option(s3[6].clone());
            config.presigned = presigned;
            config.delete_after_recognition = delete_after;
        }
        RemoteAudioConfig::AliyunOss(config) => {
            config.endpoint = oss[0].clone();
            config.bucket = oss[1].clone();
            config.access_key = oss[2].clone();
            config.secret_key = oss[3].clone();
            config.prefix = oss[4].clone();
            config.public_url_base = nonempty_option(oss[5].clone());
            config.presigned = presigned;
            config.delete_after_recognition = delete_after;
        }
    }
}

fn sync_remote_controls_from_draft(state: &mut SettingsState) {
    let (webdav, s3, oss, presigned, delete_after) = match &state.advanced_audio.remote_draft {
        RemoteAudioConfig::None => (
            vec![String::new(); REMOTE_WEBDAV_FIELDS.len()],
            vec![String::new(); REMOTE_S3_FIELDS.len()],
            vec![String::new(); REMOTE_OSS_FIELDS.len()],
            false,
            false,
        ),
        RemoteAudioConfig::Webdav(config) => (
            vec![
                config.upload_base_url.clone(),
                config.username.clone(),
                config.password.clone(),
                config.remote_path_prefix.clone(),
                config.public_download_base_url.clone(),
            ],
            vec![String::new(); REMOTE_S3_FIELDS.len()],
            vec![String::new(); REMOTE_OSS_FIELDS.len()],
            false,
            config.delete_after_recognition,
        ),
        RemoteAudioConfig::S3Compatible(config) => (
            vec![String::new(); REMOTE_WEBDAV_FIELDS.len()],
            vec![
                config.endpoint.clone(),
                config.region.clone(),
                config.bucket.clone(),
                config.access_key.clone(),
                config.secret_key.clone(),
                config.prefix.clone(),
                config.public_url_base.clone().unwrap_or_default(),
            ],
            vec![String::new(); REMOTE_OSS_FIELDS.len()],
            config.presigned,
            config.delete_after_recognition,
        ),
        RemoteAudioConfig::AliyunOss(config) => (
            vec![String::new(); REMOTE_WEBDAV_FIELDS.len()],
            vec![String::new(); REMOTE_S3_FIELDS.len()],
            vec![
                config.endpoint.clone(),
                config.bucket.clone(),
                config.access_key.clone(),
                config.secret_key.clone(),
                config.prefix.clone(),
                config.public_url_base.clone().unwrap_or_default(),
            ],
            config.presigned,
            config.delete_after_recognition,
        ),
    };
    set_remote_field_values(state, &REMOTE_WEBDAV_FIELDS, &webdav);
    set_remote_field_values(state, &REMOTE_S3_FIELDS, &s3);
    set_remote_field_values(state, &REMOTE_OSS_FIELDS, &oss);
    state.boolean_values.insert(REMOTE_PRESIGNED_KEY, presigned);
    state
        .boolean_values
        .insert(REMOTE_DELETE_AFTER_KEY, delete_after);
}

fn remote_field_values(state: &SettingsState, fields: &[(&str, &str, bool)]) -> Vec<String> {
    fields
        .iter()
        .map(|(key, _, _)| {
            state
                .controls
                .get(key)
                .map(|control| read_text(*control))
                .unwrap_or_default()
        })
        .collect()
}

fn set_remote_field_values(
    state: &SettingsState,
    fields: &[(&str, &str, bool)],
    values: &[String],
) {
    for ((key, _, _), value) in fields.iter().zip(values) {
        let Some(control) = state.controls.get(key) else {
            continue;
        };
        let text = wide(value);
        unsafe {
            let _ = SetWindowTextW(*control, PCWSTR(text.as_ptr()));
        }
    }
}

fn remote_boolean(state: &SettingsState, key: &'static str) -> bool {
    state.boolean_values.get(key).copied().unwrap_or(false)
}

fn nonempty_option(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn toggle_remote_boolean(state: &mut SettingsState, id: usize) {
    match (&mut state.advanced_audio.remote_draft, id) {
        (RemoteAudioConfig::Webdav(config), ID_REMOTE_DELETE_AFTER) => {
            config.delete_after_recognition = !config.delete_after_recognition;
        }
        (RemoteAudioConfig::S3Compatible(config), ID_REMOTE_PRESIGNED) => {
            config.presigned = !config.presigned;
        }
        (RemoteAudioConfig::S3Compatible(config), ID_REMOTE_DELETE_AFTER) => {
            config.delete_after_recognition = !config.delete_after_recognition;
        }
        (RemoteAudioConfig::AliyunOss(config), ID_REMOTE_PRESIGNED) => {
            config.presigned = !config.presigned;
        }
        (RemoteAudioConfig::AliyunOss(config), ID_REMOTE_DELETE_AFTER) => {
            config.delete_after_recognition = !config.delete_after_recognition;
        }
        _ => {}
    }
}

pub(super) fn sync_visibility(state: &SettingsState) {
    let active = GROUPS
        .get(state.active_group)
        .map(|group| group.0)
        .unwrap_or_default();
    if active != GROUP {
        pop_dynamic_label_tooltip(state);
        sync_dynamic_label_tooltip_rects(state, false, false);
        return;
    }
    let page = &state.advanced_audio;
    let remote_open = page.remote_open;
    let provider = remote_provider(&page.remote_draft);
    let summary_visible = !remote_open && page.dynamic_form.is_some() && !is_generating(state);
    let generation_status_visible = generation_status_message(state).is_some();
    set_controls_visible(&page.normal_controls, !remote_open);
    set_controls_visible(&page.generation_controls, !remote_open && !summary_visible);
    set_controls_visible(
        &[page.summary_label, page.summary_editor, page.summary_reset],
        summary_visible,
    );
    sync_dynamic_visibility(state, !remote_open && !generation_status_visible);
    if let Some(remote_button) = state.controls.get(REMOTE_OPEN_KEY) {
        set_controls_visible(
            std::slice::from_ref(remote_button),
            !remote_open && workflow_requires_remote_draft(state),
        );
    }
    set_controls_visible(&page.remote_common_controls, remote_open);
    set_controls_visible(
        &[page.remote_viewport],
        remote_open && provider != RemoteProvider::None,
    );
    set_controls_visible(
        &page.remote_webdav_controls,
        remote_open && provider == RemoteProvider::Webdav,
    );
    set_controls_visible(
        &page.remote_s3_controls,
        remote_open && provider == RemoteProvider::S3Compatible,
    );
    set_controls_visible(
        &page.remote_oss_controls,
        remote_open && provider == RemoteProvider::AliyunOss,
    );
    set_controls_visible(
        &page.remote_presigned_controls,
        remote_open
            && matches!(
                provider,
                RemoteProvider::S3Compatible | RemoteProvider::AliyunOss
            ),
    );
    set_controls_visible(
        &page.remote_cleanup_controls,
        remote_open && provider != RemoteProvider::None,
    );
    sync_generation_status_control(state, !remote_open);
}

fn generation_status_message(state: &SettingsState) -> Option<String> {
    match &state.advanced_audio.generation {
        GenerationState::NeedsMoreInformation(message) => Some(format!(
            "{}: {message}",
            state.language.text("advanced_audio_needs_more_information")
        )),
        GenerationState::Unsupported(message) => Some(format!(
            "{}: {message}",
            state.language.text("advanced_audio_unsupported")
        )),
        GenerationState::Failed(message) => Some(format!(
            "{}: {message}",
            state.language.text("advanced_audio_generation_failed")
        )),
        _ => None,
    }
}

fn sync_generation_status_control(state: &SettingsState, visible: bool) {
    let control = state.advanced_audio.generation_status;
    let message = generation_status_message(state);
    if let Some(message) = message.as_ref().filter(|_| !control.is_invalid())
        && read_text(control) != *message
    {
        unsafe {
            let _ = SetWindowTextW(control, PCWSTR(wide(message).as_ptr()));
        }
    }
    set_controls_visible(&[control], visible && message.is_some());
}

fn sync_dynamic_visibility(state: &SettingsState, visible: bool) {
    let page = &state.advanced_audio;
    let (value, secret) = current_dynamic_inputs(state);
    let page_count = page
        .dynamic_form
        .as_ref()
        .map(|form| form.pages(&page.config.values).len())
        .unwrap_or_default();
    let value_visible = visible && value.is_some();
    let secret_visible = visible && secret.is_some();
    let navigation_visible = visible && page_count > 1;
    set_controls_visible(&[page.dynamic_values_heading], value_visible);
    set_controls_visible(&[page.dynamic_value_label], value_visible);
    let active_value_control = value_visible.then(|| {
        match value
            .as_ref()
            .expect("visible dynamic value")
            .parameter_type
        {
            ParameterType::Text | ParameterType::Integer | ParameterType::Number => {
                page.dynamic_value_editor
            }
            ParameterType::Boolean => page.dynamic_boolean,
            ParameterType::Select => page.dynamic_select,
            ParameterType::MultiSelect => page.dynamic_multi_select,
            ParameterType::JsonObject => page.dynamic_json_object,
            ParameterType::JsonArray => page.dynamic_json_array,
        }
    });
    for control in [
        page.dynamic_value_editor,
        page.dynamic_boolean,
        page.dynamic_select,
        page.dynamic_multi_select,
        page.dynamic_json_object,
        page.dynamic_json_array,
    ] {
        set_controls_visible(&[control], active_value_control == Some(control));
    }
    // Dynamic dropdown panels are opened explicitly. A normal page or
    // visibility sync must never leave a stale popup above another field.
    set_controls_visible(
        &[page.dynamic_select_panel, page.dynamic_multi_select_panel],
        false,
    );
    set_controls_visible(&[page.dynamic_secrets_heading], secret_visible);
    set_controls_visible(
        &[page.dynamic_secret_label, page.dynamic_secret_editor],
        secret_visible,
    );
    set_controls_visible(
        &[page.dynamic_previous, page.dynamic_next],
        navigation_visible,
    );
    if !value_visible || !secret_visible {
        pop_dynamic_label_tooltip(state);
    }
    sync_dynamic_label_tooltip_rects(state, value_visible, secret_visible);
}

fn set_controls_visible(controls: &[HWND], visible: bool) {
    unsafe {
        for control in controls {
            if IsWindowVisible(*control).as_bool() != visible {
                let _ = ShowWindow(*control, if visible { SW_SHOW } else { SW_HIDE });
            }
        }
    }
}

fn confirm_workflow_test(state: &SettingsState, preview: &WorkflowTestPreview) -> bool {
    let targets = if preview.targets.is_empty() {
        state
            .language
            .text("advanced_audio_no_network_target")
            .into()
    } else {
        preview
            .targets
            .iter()
            .map(|target| target.display(state.language))
            .collect::<Vec<_>>()
            .join("\r\n")
    };
    let yes = state.language.text("advanced_audio_yes");
    let no = state.language.text("advanced_audio_no");
    let message = format!(
        "{}:\r\n{targets}\r\n\r\n{}: {}\r\n{}: {}\r\n{}: {}",
        state.language.text("advanced_audio_test_targets"),
        state.language.text("advanced_audio_test_remote_upload"),
        if preview.remote_upload { yes } else { no },
        state.language.text("advanced_audio_test_mode"),
        preview.mode,
        state.language.text("advanced_audio_test_realtime_replay"),
        if preview.realtime_replay { yes } else { no },
    );
    let message = wide(&message);
    let title = wide(state.language.text("advanced_audio_test_confirm"));
    let result = unsafe {
        windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
            Some(state.hwnd),
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::MB_YESNO
                | windows::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION,
        )
    };
    result == windows::Win32::UI::WindowsAndMessaging::IDYES
}

fn start_generation(state: &mut SettingsState) {
    let material = read_text(state.controls[MATERIAL_KEY]);
    let user_requirements = read_text(state.controls[USER_REQUIREMENTS_KEY]);
    if material.trim().is_empty() {
        state.advanced_audio.generation = GenerationState::Failed(
            state
                .language
                .text("advanced_audio_material_required")
                .into(),
        );
        sync_visibility(state);
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), None, true);
        }
        return;
    }
    // Generation only needs the current Rewrite draft. Do not call
    // `read_config` or `Config::validate` here: the user may be generating a
    // replacement for an incomplete Advanced Audio workflow.
    let mut config = state.runtime.config();
    config.rewrite = rewrite::read(state);
    let redacted_user_requirements =
        advanced_audio_prompt::redact_generation_material(&user_requirements);
    let redacted_vendor_material = advanced_audio_prompt::redact_generation_material(&material);
    let cancel = tokio_util::sync::CancellationToken::new();
    let worker_cancel = cancel.clone();
    let (sender, receiver) = std::sync::mpsc::channel();

    state.advanced_audio.generation_cancel = cancel;
    state.advanced_audio.generation_receiver = Some(receiver);
    state.advanced_audio.generation = GenerationState::Generating;
    state.advanced_audio.test_requested = false;
    update_controls(state);
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(state.hwnd), TIMER, 50, None);
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }

    std::thread::spawn(move || {
        let result = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(executor) => executor.block_on(advanced_audio_prompt::compile_workflow(
                config,
                redacted_user_requirements,
                redacted_vendor_material,
                &worker_cancel,
            )),
            Err(error) => Err(error.to_string()),
        };
        let _ = sender.send(result);
    });
}

fn reset_workflow_draft(state: &mut SettingsState) {
    let empty = wide("");
    unsafe {
        let _ = SetWindowTextW(
            state.controls[USER_REQUIREMENTS_KEY],
            PCWSTR(empty.as_ptr()),
        );
        let _ = SetWindowTextW(state.controls[MATERIAL_KEY], PCWSTR(empty.as_ptr()));
        let _ = SetWindowTextW(state.controls[WORKFLOW_KEY], PCWSTR(empty.as_ptr()));
    }

    state.advanced_audio.config.workflow = None;
    state.advanced_audio.config.values.clear();
    state.advanced_audio.config.secrets.clear();
    state.advanced_audio.validation = ValidationState::NotValidated;
    state.advanced_audio.generation = GenerationState::Idle;
    state.advanced_audio.test_requested = false;
    refresh_dynamic_form(state);
    update_controls(state);
    sync_visibility(state);
    unsafe {
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }
}

pub(super) fn poll_generation(state: &mut SettingsState) {
    let result = match state.advanced_audio.generation_receiver.as_ref() {
        Some(receiver) => match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("Workflow generation stopped before returning a result.".into())
            }
        },
        None => return,
    };
    state.advanced_audio.generation_receiver = None;
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(state.hwnd), TIMER);
    }

    state.advanced_audio.generation = match result {
        Ok(CompilerOutput::Ok { workflow, warnings }) => {
            let workflow_text = workflow_editor_text(Some(&workflow));
            unsafe {
                let _ = SetWindowTextW(
                    state.controls[WORKFLOW_KEY],
                    PCWSTR(wide(&workflow_text).as_ptr()),
                );
            }
            validate_workflow_draft(state);
            match &mut state.advanced_audio.validation {
                ValidationState::Valid(summary) => {
                    summary.warnings = warnings;
                    GenerationState::Idle
                }
                ValidationState::Invalid(error) => GenerationState::Failed(format!(
                    "Generated workflow failed local validation: {error}"
                )),
                ValidationState::NotValidated => GenerationState::Failed(
                    "Generated workflow could not be validated locally.".into(),
                ),
            }
        }
        Ok(CompilerOutput::NeedsMoreInformation { message }) => {
            GenerationState::NeedsMoreInformation(message)
        }
        Ok(CompilerOutput::Unsupported { message }) => GenerationState::Unsupported(message),
        Err(error) => GenerationState::Failed(error),
    };
    // The successful branch may have just replaced compiler warnings; refresh
    // after the mutable ValidationState borrow has ended.
    sync_summary_control(state);
    update_controls(state);
    sync_visibility(state);
    unsafe {
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }
}

pub(super) fn cancel_generation(state: &mut SettingsState) {
    state.advanced_audio.generation_cancel.cancel();
    state.advanced_audio.generation_receiver = None;
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(state.hwnd), TIMER);
    }
}

pub(super) fn is_generating(state: &SettingsState) -> bool {
    matches!(state.advanced_audio.generation, GenerationState::Generating)
}

pub(super) fn read(state: &SettingsState) -> Result<AdvancedAudioConfig, String> {
    let mut advanced = state.advanced_audio.config.clone();
    advanced.enabled = enabled(state);
    advanced.workflow = parse_workflow(
        state.controls[WORKFLOW_KEY],
        state.language.text("advanced_audio_workflow_json"),
    )?;
    Ok(advanced)
}

pub(super) fn enabled(state: &SettingsState) -> bool {
    state
        .boolean_values
        .get(ENABLE_KEY)
        .copied()
        .unwrap_or(false)
}

pub(super) fn update_controls(state: &SettingsState) {
    let enabled = enabled(state) && !state.saving;
    let generating = is_generating(state);
    let testing = matches!(
        state.connectivity_status,
        super::ConnectivityStatus::Testing
    );
    for key in [
        USER_REQUIREMENTS_KEY,
        MATERIAL_KEY,
        WORKFLOW_KEY,
        VALIDATE_KEY,
        GENERATE_KEY,
    ] {
        if let Some(control) = state.controls.get(key) {
            unsafe {
                let _ = EnableWindow(*control, enabled && !generating);
                let _ = InvalidateRect(Some(*control), None, true);
            }
        }
    }
    let (value, secret) = current_dynamic_inputs(state);
    let value_type = value.as_ref().map(|definition| definition.parameter_type);
    let (page, page_count) = state
        .advanced_audio
        .dynamic_form
        .as_ref()
        .map(|form| {
            (
                form.page,
                form.pages(&state.advanced_audio.config.values).len(),
            )
        })
        .unwrap_or((0, 0));
    for (control, available) in [
        (
            state.advanced_audio.dynamic_value_editor,
            matches!(
                value_type,
                Some(ParameterType::Text | ParameterType::Integer | ParameterType::Number)
            ),
        ),
        (
            state.advanced_audio.dynamic_boolean,
            value_type == Some(ParameterType::Boolean),
        ),
        (
            state.advanced_audio.dynamic_select,
            value_type == Some(ParameterType::Select),
        ),
        (
            state.advanced_audio.dynamic_multi_select,
            value_type == Some(ParameterType::MultiSelect),
        ),
        (
            state.advanced_audio.dynamic_json_object,
            value_type == Some(ParameterType::JsonObject),
        ),
        (
            state.advanced_audio.dynamic_json_array,
            value_type == Some(ParameterType::JsonArray),
        ),
        (state.advanced_audio.dynamic_secret_editor, secret.is_some()),
    ] {
        unsafe {
            let _ = EnableWindow(control, enabled && !generating && available);
            let _ = InvalidateRect(Some(control), None, true);
        }
    }
    let summary_available = state.advanced_audio.dynamic_form.is_some();
    unsafe {
        let _ = EnableWindow(
            state.advanced_audio.summary_editor,
            enabled && !generating && summary_available,
        );
        let _ = InvalidateRect(Some(state.advanced_audio.summary_editor), None, true);
    }
    if let Some(control) = state.controls.get(RESET_KEY) {
        unsafe {
            let _ = EnableWindow(
                *control,
                enabled && !generating && !testing && summary_available,
            );
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    for (control, available) in [
        (
            state.advanced_audio.dynamic_previous,
            page_count > 1 && page > 0,
        ),
        (
            state.advanced_audio.dynamic_next,
            page_count > 1 && page + 1 < page_count,
        ),
    ] {
        unsafe {
            let _ = EnableWindow(control, enabled && !generating && available);
            let _ = InvalidateRect(Some(control), None, true);
        }
    }
    if let Some(control) = state.controls.get(TEST_KEY) {
        unsafe {
            let _ = EnableWindow(*control, enabled && !generating && !testing);
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    if let Some(control) = state.controls.get(REMOTE_OPEN_KEY) {
        unsafe {
            let _ = EnableWindow(
                *control,
                enabled && !generating && workflow_requires_remote_draft(state),
            );
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    for control in state
        .advanced_audio
        .remote_common_controls
        .iter()
        .chain(state.advanced_audio.remote_webdav_controls.iter())
        .chain(state.advanced_audio.remote_s3_controls.iter())
        .chain(state.advanced_audio.remote_oss_controls.iter())
        .chain(state.advanced_audio.remote_presigned_controls.iter())
        .chain(state.advanced_audio.remote_cleanup_controls.iter())
    {
        unsafe {
            let _ = EnableWindow(*control, enabled && !generating);
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    if let Some(control) = state.controls.get(CANCEL_GENERATION_KEY) {
        unsafe {
            let _ = EnableWindow(*control, enabled && generating);
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    if let Some(control) = state.controls.get(ENABLE_KEY) {
        unsafe {
            let _ = EnableWindow(*control, !state.saving && !generating);
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    if let Some(control) = state.controls.get("__save") {
        unsafe {
            let _ = EnableWindow(*control, !state.saving && !generating);
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    sync_visibility(state);
}

pub(super) fn is_button(id: usize) -> bool {
    matches!(
        id,
        ID_VALIDATE
            | ID_TEST
            | ID_GENERATE
            | ID_CANCEL_GENERATION
            | ID_RESET
            | ID_DYNAMIC_PREVIOUS
            | ID_DYNAMIC_NEXT
            | ID_REMOTE_OPEN
            | ID_REMOTE_BACK
            | ID_REMOTE_APPLY
            | ID_REMOTE_NONE
            | ID_REMOTE_WEBDAV
            | ID_REMOTE_S3
            | ID_REMOTE_OSS
    )
}

pub(super) fn is_dynamic_select_button(id: usize) -> bool {
    matches!(id, ID_DYNAMIC_SELECT | ID_DYNAMIC_MULTI_SELECT)
}

pub(super) fn dynamic_select_is_open_for_draw(state: &SettingsState, id: usize) -> bool {
    match id {
        ID_DYNAMIC_SELECT => dynamic_select_is_open(state),
        ID_DYNAMIC_MULTI_SELECT => dynamic_multi_select_is_open(state),
        _ => false,
    }
}

pub(super) fn is_dynamic_list(id: usize) -> bool {
    matches!(id, ID_DYNAMIC_SELECT_LIST | ID_DYNAMIC_MULTI_SELECT_LIST)
}

pub(super) fn dynamic_list_item_height(id: usize, dpi: u32) -> Option<i32> {
    match id {
        ID_DYNAMIC_SELECT_LIST => Some(platform::scale(dropdown::ROW_HEIGHT, dpi)),
        ID_DYNAMIC_MULTI_SELECT_LIST => Some(platform::scale(MULTI_SELECT_ROW_HEIGHT, dpi)),
        _ => None,
    }
}

pub(super) fn draw_dynamic_list(state: &SettingsState, item: &DRAWITEMSTRUCT) -> bool {
    let id = if item.hwndItem == state.advanced_audio.dynamic_select_list {
        ID_DYNAMIC_SELECT_LIST
    } else if item.hwndItem == state.advanced_audio.dynamic_multi_select_list {
        ID_DYNAMIC_MULTI_SELECT_LIST
    } else {
        item.CtlID as usize
    };
    let Some(definition) = current_dynamic_inputs(state).0 else {
        return is_dynamic_list(id);
    };
    let matches_type = matches!(
        (id, definition.parameter_type),
        (ID_DYNAMIC_SELECT_LIST, ParameterType::Select)
            | (ID_DYNAMIC_MULTI_SELECT_LIST, ParameterType::MultiSelect)
    );
    if !matches_type {
        return is_dynamic_list(id);
    }
    let Some(option) = definition.options.get(item.itemID as usize) else {
        return true;
    };
    let selected = match definition.parameter_type {
        ParameterType::Select => {
            dynamic_input_value(&state.advanced_audio.config.values, &definition) == option.value
        }
        ParameterType::MultiSelect => item.itemState.0 & ODS_SELECTED.0 != 0,
        _ => false,
    };
    unsafe {
        dropdown::draw_row(
            item.hDC,
            item.rcItem,
            selected,
            item.itemState.0 & ODS_SELECTED.0 != 0
                || dropdown::hovered_row(item.hwndItem, item.itemID),
            state.dpi,
        );
        let mut rect = item.rcItem;
        rect.left += platform::scale(14, state.dpi);
        rect.right -= platform::scale(8, state.dpi);
        SetTextColor(
            item.hDC,
            if selected {
                rgb(231, 250, 246)
            } else {
                rgb(190, 205, 208)
            },
        );
        SetBkMode(item.hDC, TRANSPARENT);
        let old = SelectObject(item.hDC, HGDIOBJ(state.font.0));
        let mut text = wide(dynamic_option_label(option));
        let text_len = text.len().saturating_sub(1);
        DrawTextW(
            item.hDC,
            &mut text[..text_len],
            &mut rect,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        SelectObject(item.hDC, old);
    }
    true
}

pub(super) fn is_generation_status_control(state: &SettingsState, control: HWND) -> bool {
    control == state.advanced_audio.generation_status
}

pub(super) fn paint(state: &SettingsState, hdc: HDC) {
    let s = |value| platform::scale(value, state.dpi);
    if !enabled(state) {
        unsafe {
            draw_text_wrapped(
                hdc,
                state.small_font,
                state.language.text("advanced_audio_disabled_hint"),
                rgb(132, 151, 156),
                RECT {
                    left: s(CONTENT_LEFT),
                    top: s(STATUS_TOP),
                    right: s(CONTENT_LEFT + EDIT_WIDTH),
                    bottom: s(600),
                },
            );
        }
        return;
    }

    if state.advanced_audio.remote_open {
        return;
    }

    match &state.advanced_audio.generation {
        GenerationState::Idle => {}
        GenerationState::Generating => {
            paint_generation_status(
                state,
                hdc,
                state.language.text("advanced_audio_generating"),
                rgb(174, 193, 196),
            );
            return;
        }
        GenerationState::Canceled => {
            paint_generation_status(
                state,
                hdc,
                state.language.text("advanced_audio_generation_canceled"),
                rgb(221, 183, 105),
            );
            return;
        }
        GenerationState::NeedsMoreInformation(_)
        | GenerationState::Unsupported(_)
        | GenerationState::Failed(_) => return,
    }

    if state.advanced_audio.test_requested {
        paint_workflow_test_status(state, hdc);
        return;
    }

    match &state.advanced_audio.validation {
        ValidationState::NotValidated => unsafe {
            draw_text_line(
                hdc,
                state.small_font,
                state.language.text("advanced_audio_validate_hint"),
                rgb(132, 151, 156),
                RECT {
                    left: s(CONTENT_LEFT),
                    top: s(STATUS_TOP),
                    right: s(CONTENT_LEFT + EDIT_WIDTH),
                    bottom: s(600),
                },
                DT_LEFT,
            );
        },
        ValidationState::Invalid(error) => unsafe {
            draw_text_line(
                hdc,
                state.small_font,
                state.language.text("advanced_audio_validation_errors"),
                rgb(255, 112, 116),
                RECT {
                    left: s(CONTENT_LEFT),
                    top: s(STATUS_TOP - 2),
                    right: s(CONTENT_LEFT + EDIT_WIDTH),
                    bottom: s(STATUS_TOP + 16),
                },
                DT_LEFT,
            );
            draw_text_wrapped(
                hdc,
                state.small_font,
                error,
                rgb(255, 142, 145),
                RECT {
                    left: s(CONTENT_LEFT),
                    top: s(STATUS_TOP + 14),
                    right: s(CONTENT_LEFT + EDIT_WIDTH),
                    bottom: s(600),
                },
            );
        },
        ValidationState::Valid(_) => {}
    }
}

fn paint_workflow_test_status(state: &SettingsState, hdc: HDC) {
    let (message, color) = match &state.connectivity_status {
        super::ConnectivityStatus::Idle | super::ConnectivityStatus::Testing => (
            state.language.text("advanced_audio_testing").into(),
            rgb(174, 193, 196),
        ),
        super::ConnectivityStatus::Succeeded => (
            format!(
                "{} ({} ms)",
                state.language.text("advanced_audio_test_succeeded"),
                state.connectivity_elapsed
            ),
            rgb(112, 215, 195),
        ),
        super::ConnectivityStatus::Failed(error) => (
            format!(
                "{}: {error}",
                state.language.text("advanced_audio_test_failed")
            ),
            rgb(255, 142, 145),
        ),
    };
    paint_generation_status(state, hdc, &message, color);
}

fn paint_generation_status(state: &SettingsState, hdc: HDC, message: &str, color: COLORREF) {
    let s = |value| platform::scale(value, state.dpi);
    unsafe {
        draw_text_wrapped(
            hdc,
            state.small_font,
            message,
            color,
            RECT {
                left: s(CONTENT_LEFT),
                top: s(STATUS_TOP),
                right: s(CONTENT_LEFT + EDIT_WIDTH),
                bottom: s(600),
            },
        );
    }
}

pub(super) fn paint_legacy_notice(state: &SettingsState, hdc: HDC) {
    if !enabled(state) {
        return;
    }
    let s = |value| platform::scale(value, state.dpi);
    unsafe {
        draw_text_wrapped(
            hdc,
            state.small_font,
            state.language.text("advanced_audio_legacy_notice"),
            rgb(221, 183, 105),
            RECT {
                left: s(CONTENT_LEFT),
                top: s(562),
                right: s(EDIT_LEFT + EDIT_WIDTH),
                bottom: s(598),
            },
        );
    }
}

fn summary_rows(summary: &WorkflowSummary, language: Language) -> Vec<String> {
    let mut rows = Vec::new();
    match (&summary.name, &summary.version) {
        (Some(name), Some(version)) => rows.push(format!("Workflow: {name} · v{version}")),
        (Some(name), None) => rows.push(format!("Workflow: {name}")),
        (None, Some(version)) => rows.push(format!("Workflow schema: v{version}")),
        (None, None) => {}
    }
    if let Some(mode) = &summary.mode {
        rows.push(format!("Mode: {mode}"));
    }
    rows.push(format!("Audio delivery: {}", summary.audio_delivery));
    rows.extend(summary.structure.iter().cloned());
    if !summary.warnings.is_empty() {
        rows.extend(
            summary
                .warnings
                .iter()
                .map(|warning| format!("{}: {warning}", language.text("advanced_audio_warnings"))),
        );
    }
    if !summary.required_values.is_empty() {
        rows.extend(summary.required_values.iter().map(|value| {
            format!(
                "{}: {value}",
                language.text("advanced_audio_required_values")
            )
        }));
    }
    if !summary.required_secrets.is_empty() {
        rows.extend(summary.required_secrets.iter().map(|secret| {
            format!(
                "{}: {secret}",
                language.text("advanced_audio_required_secrets")
            )
        }));
    }
    rows
}

fn workflow_summary(
    workflow: &AdvancedAudioWorkflow,
    remote_audio: &RemoteAudioConfig,
) -> WorkflowSummary {
    WorkflowSummary {
        name: Some(workflow.name.clone()),
        mode: Some(workflow.recognition.mode_name().into()),
        version: Some(workflow.schema_version.0.to_string()),
        audio_delivery: audio_delivery_name(&workflow.audio.delivery).into(),
        structure: workflow_structure(workflow),
        warnings: Vec::new(),
        required_values: workflow
            .parameters
            .iter()
            .filter(|definition| definition.required && definition.default.is_none())
            .map(|definition| declaration_name(&definition.label, &definition.id))
            .collect(),
        required_secrets: workflow
            .secrets
            .iter()
            .filter(|definition| definition.required)
            .map(|definition| declaration_name(&definition.label, &definition.id))
            .collect(),
        test_preview: workflow_test_preview(workflow, remote_audio),
    }
}

fn audio_delivery_name(delivery: &AudioDelivery) -> &'static str {
    match delivery {
        AudioDelivery::MultipartFile => "multipart_file",
        AudioDelivery::RawAudio => "raw_audio",
        AudioDelivery::Base64 => "base64",
        AudioDelivery::DataUri => "data_uri",
        AudioDelivery::PublicHttpsUrl => "public_https_url",
        AudioDelivery::CloudUri => "cloud_uri",
        AudioDelivery::ProviderUpload => "provider_upload",
        AudioDelivery::RealtimeChunks => "realtime_chunks",
    }
}

fn workflow_structure(workflow: &AdvancedAudioWorkflow) -> Vec<String> {
    match &workflow.recognition {
        AdvancedRecognition::Request {
            request,
            final_text,
        } => vec![
            format!("Request stage: {}", stage_summary(request)),
            format!("Final text: {}", extractor_summary(final_text)),
        ],
        AdvancedRecognition::RequestStream { request, stream } => vec![format!(
            "Stream stage: {} · {:?}",
            stage_summary(request),
            stream.format
        )],
        AdvancedRecognition::AsyncPoll {
            prepare,
            submit,
            poll,
            result_steps,
            final_text,
            ..
        } => {
            let mut rows = Vec::new();
            if let Some(prepare) = prepare {
                rows.push(format!("Prepare stage: {}", stage_summary(prepare)));
            }
            rows.push(format!("Submit stage: {}", stage_summary(submit)));
            if let Some(poll) = poll {
                rows.push(format!(
                    "Poll stage: {} · {} ms / {} ms",
                    stage_summary(&poll.request),
                    poll.interval_ms,
                    poll.timeout_ms,
                ));
                rows.push(format!(
                    "Pending: {}",
                    poll_conditions_summary(&poll.pending)
                ));
                rows.push(format!(
                    "Success: {}",
                    poll_conditions_summary(&poll.success)
                ));
                rows.push(format!(
                    "Failure: {}",
                    poll_conditions_summary(&poll.failure)
                ));
            }
            for (index, result) in result_steps.iter().enumerate() {
                rows.push(format!(
                    "Result stage {}: {}",
                    index + 1,
                    stage_summary(result)
                ));
            }
            rows.push(format!("Final text: {}", extractor_summary(final_text)));
            rows
        }
        AdvancedRecognition::RealtimeSession { realtime } => {
            let partial = realtime
                .receive_rules
                .iter()
                .filter(|rule| {
                    matches!(
                        rule.action,
                        StreamAction::AppendDelta
                            | StreamAction::ReplacePartial
                            | StreamAction::CommitSegment
                    )
                })
                .map(stream_rule_summary)
                .collect::<Vec<_>>();
            let final_rules = realtime
                .receive_rules
                .iter()
                .filter(|rule| {
                    matches!(
                        rule.action,
                        StreamAction::SetFinalText | StreamAction::Complete
                    )
                })
                .map(stream_rule_summary)
                .collect::<Vec<_>>();
            vec![
                format!(
                    "Realtime: {:?} · {} · {} Hz · {} ch · {} ms · {:?}",
                    realtime.transport,
                    realtime.audio_stream.codec,
                    realtime.audio_stream.sample_rate,
                    realtime.audio_stream.channels,
                    realtime.audio_stream.chunk_duration_ms,
                    realtime.audio_stream.pacing,
                ),
                format!("Partial: {}", summary_or_none(&partial)),
                format!(
                    "Final: {} · {}",
                    summary_or_none(&final_rules),
                    realtime_completion_summary(realtime),
                ),
                "Retry: full local recording from offset zero in a new session".into(),
            ]
        }
    }
}

fn stage_summary(stage: &HttpStage) -> String {
    format!("{:?} {}", stage.method, stage.url)
}

fn extractor_summary(extractor: &ResponseExtractor) -> String {
    match extractor {
        ResponseExtractor::JsonPath { path } => format!("json_path {path}"),
        ResponseExtractor::Header { name } => format!("header {name}"),
        ResponseExtractor::PlainBody => "plain_body".into(),
        ResponseExtractor::Status => "status".into(),
    }
}

fn poll_conditions_summary(conditions: &[PollCondition]) -> String {
    let conditions = conditions
        .iter()
        .map(|condition| {
            let expected = if !condition.values.is_empty() {
                condition.values.join(", ")
            } else {
                condition.value.clone().unwrap_or_default()
            };
            let expected = (!expected.is_empty()).then_some(format!(" · {expected}"));
            format!(
                "{} · {:?}{}",
                extractor_summary(&condition.from),
                condition.operator,
                expected.unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    summary_or_none(&conditions)
}

fn stream_rule_summary(rule: &dictate_core::advanced_audio::schema::StreamRule) -> String {
    let mut fields = vec![format!("{:?}", rule.action)];
    if let Some(event) = &rule.event {
        fields.push(format!("event={event}"));
    }
    if let Some(path) = &rule.path {
        fields.push(format!("path={path}"));
    }
    if let Some(equals) = &rule.equals {
        fields.push(format!("equals={equals}"));
    }
    fields.join(" · ")
}

fn realtime_completion_summary(
    realtime: &dictate_core::advanced_audio::schema::RealtimeWorkflow,
) -> String {
    let completion = &realtime.completion;
    let mut fields = Vec::new();
    if let Some(event) = &completion.event {
        fields.push(format!("event={event}"));
    }
    if let Some(path) = &completion.path {
        fields.push(format!("path={path}"));
    }
    if let Some(equals) = &completion.equals {
        fields.push(format!("equals={equals}"));
    }
    if fields.is_empty() {
        "completion rule: none".into()
    } else {
        format!("completion: {}", fields.join(" · "))
    }
}

fn summary_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".into()
    } else {
        values.join("; ")
    }
}

fn workflow_test_preview(
    workflow: &AdvancedAudioWorkflow,
    remote_audio: &RemoteAudioConfig,
) -> WorkflowTestPreview {
    let mut targets = BTreeSet::new();
    match &workflow.recognition {
        AdvancedRecognition::Request { request, .. }
        | AdvancedRecognition::RequestStream { request, .. } => {
            add_target(&mut targets, &request.url);
        }
        AdvancedRecognition::AsyncPoll {
            prepare,
            submit,
            poll,
            result_steps,
            ..
        } => {
            if let Some(prepare) = prepare {
                add_target(&mut targets, &prepare.url);
            }
            add_target(&mut targets, &submit.url);
            if let Some(poll) = poll {
                add_target(&mut targets, &poll.request.url);
            }
            for step in result_steps {
                add_target(&mut targets, &step.url);
            }
        }
        AdvancedRecognition::RealtimeSession { realtime } => {
            add_target(&mut targets, &realtime.connect.url);
        }
    }

    let remote_upload = workflow_requires_remote(workflow);
    if remote_upload {
        match remote_audio {
            RemoteAudioConfig::Webdav(config) => {
                add_target(&mut targets, &config.upload_base_url);
                // The ASR service fetches this separately configured public
                // location after upload, so make it visible before Test.
                add_target(&mut targets, &config.public_download_base_url);
            }
            RemoteAudioConfig::S3Compatible(config) => {
                add_target(&mut targets, &config.endpoint);
                if let Some(public_url_base) = &config.public_url_base {
                    add_target(&mut targets, public_url_base);
                }
            }
            RemoteAudioConfig::AliyunOss(config) => {
                add_target(&mut targets, &config.endpoint);
                if let Some(public_url_base) = &config.public_url_base {
                    add_target(&mut targets, public_url_base);
                }
            }
            RemoteAudioConfig::None => {}
        }
    }

    WorkflowTestPreview {
        targets: targets.into_iter().collect(),
        remote_upload,
        mode: workflow.recognition.mode_name().into(),
        realtime_replay: matches!(
            &workflow.recognition,
            AdvancedRecognition::RealtimeSession { .. }
        ),
    }
}

fn add_target(targets: &mut BTreeSet<WorkflowTestTarget>, url: &str) {
    if let Some(host) = url_target_host(url) {
        targets.insert(WorkflowTestTarget::Host(host));
    } else if is_complete_capture_url(url) {
        targets.insert(WorkflowTestTarget::DynamicHttpUrl);
    }
}

fn is_complete_capture_url(url: &str) -> bool {
    url.strip_prefix("{{capture:")
        .and_then(|name| name.strip_suffix("}}"))
        .is_some_and(dictate_core::advanced_audio::template::is_identifier)
}

fn url_target_host(url: &str) -> Option<String> {
    let authority_and_path = url.trim().split_once("://")?.1;
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?
        .trim();
    (!authority.is_empty()).then_some(authority.into())
}

fn declaration_name(label: &str, id: &str) -> String {
    if label == id {
        id.into()
    } else {
        format!("{label} ({id})")
    }
}

fn parse_workflow(hwnd: HWND, label: &str) -> Result<Option<AdvancedAudioWorkflow>, String> {
    let text = read_text(hwnd);
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| format!("{label}: {error}"))
}

fn workflow_editor_text(workflow: Option<&AdvancedAudioWorkflow>) -> String {
    workflow.map_or_else(String::new, |workflow| {
        serde_json::to_string_pretty(workflow)
            .unwrap_or_default()
            .replace('\n', "\r\n")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_core::advanced_audio::schema::{
        AudioSpec, Capture, HttpMethod, PollStage, WorkflowSchemaVersion,
    };
    use std::collections::BTreeMap;

    fn stage(url: &str) -> HttpStage {
        HttpStage {
            method: HttpMethod::Get,
            url: url.into(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            body: Default::default(),
            accepted_statuses: vec![200],
            signer: Default::default(),
            captures: Vec::new(),
        }
    }

    #[test]
    fn workflow_test_preview_lists_complete_dynamic_capture_urls() {
        let mut poll_request = stage("https://asr.example.test/tasks/123");
        poll_request.captures.push(Capture {
            id: "result_url".into(),
            from: ResponseExtractor::JsonPath {
                path: "$.output.result_url".into(),
            },
            sensitive: false,
        });
        let workflow = AdvancedAudioWorkflow {
            schema_version: WorkflowSchemaVersion(1),
            name: "Dynamic result URL".into(),
            parameters: Vec::new(),
            secrets: Vec::new(),
            audio: AudioSpec {
                delivery: AudioDelivery::MultipartFile,
                mime: None,
            },
            recognition: AdvancedRecognition::AsyncPoll {
                prepare: None,
                submit: Box::new(stage("https://asr.example.test/submit")),
                poll: Some(Box::new(PollStage {
                    request: poll_request,
                    interval_ms: 1_000,
                    timeout_ms: 60_000,
                    pending: Vec::new(),
                    success: Vec::new(),
                    failure: Vec::new(),
                })),
                result_steps: vec![stage("{{capture:result_url}}")],
                final_text: ResponseExtractor::PlainBody,
            },
        };

        let preview = workflow_test_preview(&workflow, &RemoteAudioConfig::None);

        assert_eq!(
            preview.targets,
            vec![
                WorkflowTestTarget::Host("asr.example.test".into()),
                WorkflowTestTarget::DynamicHttpUrl,
            ]
        );
        assert_eq!(
            preview.targets[1].display(Language::English),
            "HTTP(S) URL dynamically returned by the service (host unknown)"
        );
        assert_eq!(
            preview.targets[1].display(Language::Chinese),
            "服务端动态返回的 HTTP(S) URL（主机未知）"
        );
    }

    fn dynamic_input(
        id: &str,
        parameter_type: ParameterType,
        default: Option<&str>,
        visible_when: Option<VisibilityCondition>,
    ) -> DynamicInput {
        DynamicInput {
            id: id.into(),
            label: id.into(),
            default: default.map(Into::into),
            parameter_type,
            options: Vec::new(),
            visible_when,
        }
    }

    #[test]
    fn conditional_dynamic_parameters_recompute_without_clearing_hidden_values() {
        let form = DynamicForm {
            parameters: vec![
                dynamic_input("enable_extra", ParameterType::Boolean, Some("false"), None),
                dynamic_input(
                    "extra_value",
                    ParameterType::Text,
                    None,
                    Some(VisibilityCondition {
                        parameter: "enable_extra".into(),
                        equals: Some("true".into()),
                        one_of: Vec::new(),
                    }),
                ),
            ],
            secrets: Vec::new(),
            page: 0,
        };
        let mut values = BTreeMap::from([("extra_value".into(), "retained".into())]);

        assert_eq!(form.visible_parameter_indices(&values), vec![0]);
        assert_eq!(values.get("extra_value"), Some(&"retained".into()));

        values.insert("enable_extra".into(), "true".into());
        assert_eq!(form.visible_parameter_indices(&values), vec![0, 1]);

        values.insert("enable_extra".into(), "false".into());
        assert_eq!(form.visible_parameter_indices(&values), vec![0]);
        assert_eq!(values.get("extra_value"), Some(&"retained".into()));
    }

    #[test]
    fn only_condition_sources_require_dynamic_layout_refresh() {
        let form = DynamicForm {
            parameters: vec![
                dynamic_input("mode", ParameterType::Select, None, None),
                dynamic_input("note", ParameterType::Text, None, None),
                dynamic_input(
                    "extra",
                    ParameterType::Text,
                    None,
                    Some(VisibilityCondition {
                        parameter: "mode".into(),
                        equals: Some("advanced".into()),
                        one_of: Vec::new(),
                    }),
                ),
            ],
            secrets: Vec::new(),
            page: 0,
        };

        assert!(form.has_visibility_dependents("mode"));
        assert!(!form.has_visibility_dependents("note"));
        assert!(!form.has_visibility_dependents("extra"));
    }

    #[test]
    fn select_and_multi_select_receive_dedicated_pages() {
        let form = DynamicForm {
            parameters: vec![
                dynamic_input("text", ParameterType::Text, None, None),
                dynamic_input("choice", ParameterType::Select, None, None),
                dynamic_input("choices", ParameterType::MultiSelect, None, None),
            ],
            secrets: vec![dynamic_input("secret", ParameterType::Text, None, None)],
            page: 0,
        };
        let pages = form.pages(&BTreeMap::new());

        assert_eq!(
            pages
                .iter()
                .map(|page| (page.parameter_index, page.secret_index))
                .collect::<Vec<_>>(),
            vec![(Some(0), Some(0)), (Some(1), None), (Some(2), None)]
        );
    }

    #[test]
    fn json_inputs_receive_dedicated_pages_and_preserve_raw_text() {
        let form = DynamicForm {
            parameters: vec![
                dynamic_input("text", ParameterType::Text, None, None),
                dynamic_input("hotwords", ParameterType::JsonObject, None, None),
                dynamic_input("language_hints", ParameterType::JsonArray, None, None),
            ],
            secrets: vec![dynamic_input("secret", ParameterType::Text, None, None)],
            page: 0,
        };
        let pages = form.pages(&BTreeMap::new());

        assert_eq!(
            pages
                .iter()
                .map(|page| (page.parameter_index, page.secret_index))
                .collect::<Vec<_>>(),
            vec![(Some(0), Some(0)), (Some(1), None), (Some(2), None)]
        );

        let raw_object = "{\r\n  \"term\": [\"alpha\", \"beta\"]\r\n}";
        let raw_array = "[\r\n  \"zh\",\r\n  \"en\"\r\n]";
        let mut values = BTreeMap::new();
        store_dynamic_input_value(&mut values, "hotwords", raw_object.into());
        store_dynamic_input_value(&mut values, "language_hints", raw_array.into());
        assert_eq!(values.get("hotwords"), Some(&raw_object.into()));
        assert_eq!(values.get("language_hints"), Some(&raw_array.into()));
    }
}
