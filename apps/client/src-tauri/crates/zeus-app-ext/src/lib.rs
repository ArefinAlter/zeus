//! Registers every extension module of the Zeus app. The desktop shell and the test harness both call [`install`].
//!
//! Each feature crate adds one line here. Order matters only for overlapping routes, which no module should have.

#![forbid(unsafe_code)]

use std::sync::Arc;

use zeus_app::App;

/// Register every extension module on `app`. Call inside a Tokio runtime, once, right after `App::start`.
pub fn install(app: &Arc<App>) {
    app.add_extension(zeus_app_index::Module::new(app));
    app.add_extension(zeus_app_grab::Module::new(app));
    app.add_extension(zeus_app_agent::Module::new(app));
    app.add_extension(zeus_app_integration::Module::new(app));
}
