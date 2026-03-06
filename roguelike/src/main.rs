#[cfg(not(feature = "windowed"))]
use std::time::Duration;

use bevy::prelude::*;
use bevy_ratatui::RatatuiPlugins;

use roguelike::plugins::RoguelikePlugin;

fn main() {
    let mut app = App::new();

    #[cfg(not(feature = "windowed"))]
    app.add_plugins((
        MinimalPlugins.set(bevy::app::ScheduleRunnerPlugin::run_loop(
            Duration::from_secs_f32(1. / 60.),
        )),
        RatatuiPlugins::default(),
    ));

    #[cfg(feature = "windowed")]
    app.add_plugins((
        DefaultPlugins.set(ImagePlugin::default_nearest()),
        RatatuiPlugins::default(),
    ));

    app.add_plugins(RoguelikePlugin).run();
}
