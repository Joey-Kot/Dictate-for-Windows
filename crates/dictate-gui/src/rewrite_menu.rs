//! Nonactivating Rewrite flyout. Layout is pure DIP geometry; Windows draws at
//! the pointer monitor's physical DPI, independently of floating-window scale.
mod layout;

#[cfg(windows)]
mod backdrop;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod surface;
#[cfg(windows)]
mod theme;
#[cfg(windows)]
pub(crate) use native::*;
