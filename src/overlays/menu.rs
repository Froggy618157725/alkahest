use crate::{
    camera::FpsCamera,
    ecs::{
        components::Ruler,
        resources::SelectedEntity,
        tags::{EntityTag, Tags},
    },
    icons::ICON_RULER_SQUARE,
    map::MapDataList,
};

use super::gui::Overlay;

pub struct MenuBar;

impl Overlay for MenuBar {
    fn draw(
        &mut self,
        ctx: &egui::Context,
        _window: &winit::window::Window,
        resources: &mut crate::resources::Resources,
        _gui: super::gui::GuiContext<'_>,
    ) -> bool {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Utility", |ui| {
                    if ui.button(format!("{} Ruler", ICON_RULER_SQUARE)).clicked() {
                        let mut maps = resources.get_mut::<MapDataList>().unwrap();

                        if let Some(map) = maps.current_map_mut() {
                            let camera = resources.get::<FpsCamera>().unwrap();
                            let position_base = camera.position + camera.front * 15.0;
                            let e = map.scene.spawn((
                                Ruler {
                                    start: position_base + camera.right * 10.0,
                                    end: position_base - camera.right * 10.0,
                                    ..Default::default()
                                },
                                Tags([EntityTag::Utility].into_iter().collect()),
                            ));

                            if let Some(mut se) = resources.get_mut::<SelectedEntity>() {
                                se.0 = Some(e);
                            }

                            ui.close_menu();
                        }
                    }
                });
            });
        });

        true
    }
}