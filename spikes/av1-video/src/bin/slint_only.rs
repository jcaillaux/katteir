//! Size baseline: the spike's window with the same Slint features and the
//! OpenGL renderer, but no video and no dav1d. Opens for one second, then quits.

use std::time::Duration;

use slint::ComponentHandle;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint::BackendSelector::new().require_opengl_es().select()?;
    let window = SpikeWindow::new()?;
    window.set_stats("size baseline: Slint only".into());
    let quit_timer = slint::Timer::default();
    quit_timer.start(slint::TimerMode::SingleShot, Duration::from_secs(1), || {
        slint::quit_event_loop().expect("event loop running");
    });
    window.run()?;
    Ok(())
}
