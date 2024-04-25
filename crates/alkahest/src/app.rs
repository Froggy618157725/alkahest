use std::sync::Arc;

use alkahest_data::{geometry::EPrimitiveType, technique::StateSelection, tfx::TfxRenderStage};
use alkahest_renderer::{
    camera::{Camera, Viewport},
    ecs::{
        dynamic_geometry::{draw_dynamic_model_system, update_dynamic_model_system, DynamicModel},
        light::draw_light_system,
        static_geometry::{
            draw_static_instances_system, update_static_instances_system, StaticModel,
        },
        terrain::draw_terrain_patches_system,
        Scene,
    },
    gpu::{buffer::ConstantBuffer, GpuContext},
    input::InputState,
    loaders::{map_tmp::load_map, AssetManager},
    tfx::{
        externs,
        externs::{ExternStorage, Frame},
        gbuffer::GBuffer,
        globals::RenderGlobals,
        scope::{ScopeFrame, ScopeInstances},
        view::View,
    },
};
use anyhow::Context;
use destiny_pkg::TagHash;
use egui::{Key, KeyboardShortcut, Modifiers};
use glam::{Mat4, Vec3, Vec4};
use tokio::time::Instant;
use windows::{core::HRESULT, Win32::Graphics::Direct3D11::D3D11_CLEAR_DEPTH};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::WindowEvent,
    event_loop::EventLoop,
    platform::run_on_demand::EventLoopExtRunOnDemand,
};

use crate::{
    config,
    gui::context::{GuiContext, GuiViewManager},
    resources::Resources,
    ApplicationArgs,
};

pub struct AlkahestApp {
    pub window: winit::window::Window,
    pub event_loop: EventLoop<()>,

    pub gctx: Arc<GpuContext>,
    pub gui: GuiContext,
    pub resources: Resources,
    pub asset_manager: AssetManager,

    tmp_gbuffers: GBuffer,
    map: Scene,
    rglobals: RenderGlobals,
    camera: Camera,
    frame_cbuffer: ConstantBuffer<ScopeFrame>,
    time: Instant,
    delta_time: Instant,
    last_cursor_pos: Option<PhysicalPosition<f64>>,
}

impl AlkahestApp {
    pub fn new(
        event_loop: EventLoop<()>,
        icon: &winit::window::Icon,
        args: crate::ApplicationArgs,
    ) -> Self {
        let window = winit::window::WindowBuilder::new()
            .with_title("Alkahest")
            .with_inner_size(config::with(|c| {
                PhysicalSize::new(c.window.width, c.window.height)
            }))
            .with_position(config::with(|c| {
                PhysicalPosition::new(c.window.pos_x, c.window.pos_y)
            }))
            .with_maximized(config!().window.maximised)
            .with_fullscreen(if config!().window.fullscreen {
                Some(winit::window::Fullscreen::Borderless(None))
            } else {
                None
            })
            .with_window_icon(Some(icon.clone()))
            .build(&event_loop)
            .unwrap();

        puffin::set_scopes_on(true);

        let gctx = Arc::new(GpuContext::create(&window).unwrap());
        let gui = GuiContext::create(&window, gctx.clone());
        let mut resources = Resources::default();
        resources.insert(GuiViewManager::with_default_views());
        resources.insert(ExternStorage::default());
        resources.insert(InputState::default());
        resources.insert(args);

        let mut asset_manager = AssetManager::new(gctx.clone());
        let rglobals = RenderGlobals::load(gctx.clone()).expect("Failed to load render globals");
        asset_manager.block_until_idle();

        let mut camera = Camera::new_fps(Viewport {
            size: glam::UVec2::new(1920, 1080),
            origin: glam::UVec2::new(0, 0),
        });

        let frame_cbuffer = ConstantBuffer::create(gctx.clone(), None).unwrap();

        let map = load_map(
            gctx.clone(),
            &mut asset_manager,
            resources
                .get::<ApplicationArgs>()
                .map
                .unwrap_or(TagHash(u32::from_be(0x217EBB80))),
        )
        .unwrap();

        update_static_instances_system(&map);
        update_dynamic_model_system(&map);

        Self {
            tmp_gbuffers: GBuffer::create(
                (window.inner_size().width, window.inner_size().height),
                gctx.clone(),
            )
            .unwrap(),
            frame_cbuffer,
            map,

            window,
            event_loop,
            gctx,
            gui,
            resources,
            asset_manager,
            rglobals,
            camera,
            time: Instant::now(),
            delta_time: Instant::now(),
            last_cursor_pos: None,
        }
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        let AlkahestApp {
            window,
            event_loop,
            gui,
            gctx,
            resources,
            asset_manager,
            tmp_gbuffers,
            rglobals,
            camera,
            time,
            delta_time,
            last_cursor_pos,
            frame_cbuffer,
            map,
            ..
        } = self;

        event_loop.run_on_demand(move |event, target| {
            if let winit::event::Event::WindowEvent { event, .. } = event {
                let egui_event_response = gui.handle_event(window, &event);
                if !egui_event_response.consumed {
                    resources.get_mut::<InputState>().handle_event(&event);
                }

                match event {
                    WindowEvent::CloseRequested => {
                        target.exit();
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        if let Some(ref mut p) = last_cursor_pos {
                            let delta = (position.x - p.x, position.y - p.y);
                            let input = resources.get::<InputState>();
                            if (input.mouse_left() | input.mouse_middle())
                                && !egui_event_response.consumed
                            {
                                // let mut camera = resources.get_mut::<FpsCamera>().unwrap();
                                camera.update_mouse((delta.0 as f32, delta.1 as f32).into(), 0.0);

                                // Wrap the cursor around if it goes out of bounds
                                let window_dims = window.inner_size();
                                let window_dims =
                                    (window_dims.width as i32, window_dims.height as i32);
                                let cursor_pos = (position.x as i32, position.y as i32);
                                let mut new_cursor_pos = cursor_pos;

                                if cursor_pos.0 <= 0 {
                                    new_cursor_pos.0 = window_dims.0;
                                } else if cursor_pos.0 >= (window_dims.0 - 1) {
                                    new_cursor_pos.0 = 0;
                                }

                                if cursor_pos.1 <= 0 {
                                    new_cursor_pos.1 = window_dims.1;
                                } else if cursor_pos.1 >= window_dims.1 {
                                    new_cursor_pos.1 = 0;
                                }

                                if new_cursor_pos != cursor_pos {
                                    window
                                        .set_cursor_position(PhysicalPosition::new(
                                            new_cursor_pos.0 as f64,
                                            new_cursor_pos.1 as f64,
                                        ))
                                        .ok();
                                }
                                *last_cursor_pos = Some(PhysicalPosition::new(
                                    new_cursor_pos.0 as f64,
                                    new_cursor_pos.1 as f64,
                                ));

                                window.set_cursor_visible(false);
                            } else {
                                window.set_cursor_visible(true);
                                *last_cursor_pos = Some(position);
                            }
                        } else {
                            window.set_cursor_visible(true);
                            *last_cursor_pos = Some(position);
                        }
                    }
                    WindowEvent::Resized(new_dims) => {
                        let _ = gui
                            .renderer
                            .resize_buffers(&gctx.swap_chain, || {
                                gctx.resize_swapchain(new_dims.width, new_dims.height);
                                HRESULT(0)
                            })
                            .expect("Failed to resize buffers");

                        tmp_gbuffers
                            .resize((new_dims.width, new_dims.height))
                            .expect("Failed to resize GBuffer");
                        camera.set_viewport(Viewport {
                            size: glam::UVec2::new(new_dims.width, new_dims.height),
                            origin: glam::UVec2::ZERO,
                        });
                    }
                    WindowEvent::RedrawRequested => {
                        let delta_f32 = delta_time.elapsed().as_secs_f32();
                        *delta_time = Instant::now();
                        asset_manager.poll();

                        if gui.input_mut(|i| {
                            i.consume_shortcut(&KeyboardShortcut::new(Modifiers::ALT, Key::Enter))
                        }) {
                            if window.fullscreen().is_some() {
                                let _ = window.set_fullscreen(None);
                            } else {
                                let _ = window.set_fullscreen(Some(
                                    winit::window::Fullscreen::Borderless(window.current_monitor()),
                                ));
                            }

                            config::with_mut(|c| {
                                c.window.fullscreen = window.fullscreen().is_some();
                            });
                        }

                        gctx.begin_frame();
                        //
                        unsafe {
                            gctx.context().OMSetRenderTargets(
                                Some(&[
                                    Some(tmp_gbuffers.rt0.render_target.clone()),
                                    Some(tmp_gbuffers.rt1.render_target.clone()),
                                    Some(tmp_gbuffers.rt2.render_target.clone()),
                                ]),
                                &tmp_gbuffers.depth.view,
                            );
                            gctx.context().ClearRenderTargetView(
                                &tmp_gbuffers.rt0.render_target,
                                &[0.0, 0.0, 0.0, 0.0],
                            );
                            gctx.context().ClearRenderTargetView(
                                &tmp_gbuffers.rt1.render_target,
                                &[0.0, 0.0, 0.0, 0.0],
                            );
                            gctx.context().ClearRenderTargetView(
                                &tmp_gbuffers.rt2.render_target,
                                &[1.0, 0.5, 1.0, 0.0],
                            );
                            gctx.context().ClearDepthStencilView(
                                &tmp_gbuffers.depth.view,
                                D3D11_CLEAR_DEPTH.0 as _,
                                0.0,
                                0,
                            );

                            gctx.context()
                                .OMSetDepthStencilState(&tmp_gbuffers.depth.state, 0);

                            frame_cbuffer
                                .write(&ScopeFrame {
                                    game_time: time.elapsed().as_secs_f32(),
                                    render_time: time.elapsed().as_secs_f32(),
                                    delta_game_time: delta_f32,
                                    ..Default::default()
                                })
                                .unwrap();
                        }

                        {
                            let mut externs = resources.get_mut::<ExternStorage>();
                            externs.frame = Some(Frame {
                                unk00: time.elapsed().as_secs_f32(),
                                unk04: time.elapsed().as_secs_f32(),
                                // Light mul (exposure related)
                                unk1c: 1.0,
                                specular_lobe_3d_lookup: rglobals
                                    .textures
                                    .specular_lobe_3d_lookup
                                    .view
                                    .clone()
                                    .into(),
                                specular_lobe_lookup: rglobals
                                    .textures
                                    .specular_lobe_lookup
                                    .view
                                    .clone()
                                    .into(),
                                specular_tint_lookup: rglobals
                                    .textures
                                    .specular_tint_lookup
                                    .view
                                    .clone()
                                    .into(),
                                iridescence_lookup: rglobals
                                    .textures
                                    .iridescence_lookup
                                    .view
                                    .clone()
                                    .into(),

                                unk1a0: Vec4::ZERO,
                                unk1b0: Vec4::ONE,
                                ..Default::default()
                            });
                            externs.view = Some({
                                let mut view = externs::View::default();
                                camera.update(&resources.get::<InputState>(), delta_f32, true);
                                camera.update_extern(&mut view);
                                view
                            });

                            externs.transparent = Some(externs::Transparent {
                                unk00: tmp_gbuffers.staging_clone.view.clone().into(),
                                unk08: gctx.grey_texture.view.clone().into(),
                                unk10: tmp_gbuffers.staging_clone.view.clone().into(),
                                unk18: gctx.grey_texture.view.clone().into(),
                                unk20: gctx.grey_texture.view.clone().into(),
                                unk28: gctx.grey_texture.view.clone().into(),
                                unk30: gctx.grey_texture.view.clone().into(),
                                unk38: gctx.grey_texture.view.clone().into(),
                                unk40: gctx.grey_texture.view.clone().into(),
                                unk48: gctx.grey_texture.view.clone().into(),
                                unk50: gctx.grey_texture.view.clone().into(),
                                unk58: gctx.grey_texture.view.clone().into(),
                                unk60: gctx.grey_texture.view.clone().into(),
                                ..Default::default()
                            });
                            externs.deferred = Some(externs::Deferred {
                                unk00: Vec4::new(0.0, 1. / 0.0001, 0.0, 0.0),
                                deferred_depth: tmp_gbuffers.depth.texture_copy_view.clone().into(),
                                deferred_rt0: tmp_gbuffers.rt0.view.clone().into(),
                                deferred_rt1: tmp_gbuffers.rt1.view.clone().into(),
                                deferred_rt2: tmp_gbuffers.rt2.view.clone().into(),
                                light_diffuse: tmp_gbuffers.light_diffuse.view.clone().into(),
                                light_specular: tmp_gbuffers.light_specular.view.clone().into(),
                                light_ibl_specular: tmp_gbuffers
                                    .light_ibl_specular
                                    .view
                                    .clone()
                                    .into(),
                                ..Default::default()
                            });

                            rglobals
                                .scopes
                                .frame
                                .bind(gctx, &asset_manager, &externs)
                                .unwrap();
                            rglobals
                                .scopes
                                .view
                                .bind(gctx, &asset_manager, &externs)
                                .unwrap();

                            unsafe {
                                gctx.context().VSSetConstantBuffers(
                                    13,
                                    Some(&[Some(frame_cbuffer.buffer().clone())]),
                                );
                                gctx.context().PSSetConstantBuffers(
                                    13,
                                    Some(&[Some(frame_cbuffer.buffer().clone())]),
                                );
                            }

                            gctx.current_states.store(StateSelection::new(
                                Some(0),
                                Some(0),
                                Some(2),
                                Some(0),
                            ));

                            draw_terrain_patches_system(&gctx, &map, asset_manager, &externs);

                            draw_static_instances_system(
                                &gctx,
                                &map,
                                asset_manager,
                                &externs,
                                TfxRenderStage::GenerateGbuffer,
                            );

                            draw_dynamic_model_system(
                                &gctx,
                                &map,
                                asset_manager,
                                &externs,
                                TfxRenderStage::GenerateGbuffer,
                            );

                            tmp_gbuffers.rt1.copy_to(&tmp_gbuffers.rt1_clone);
                            tmp_gbuffers.depth.copy_depth();

                            externs.decal = Some(externs::Decal {
                                unk08: tmp_gbuffers.rt1_clone.view.clone().into(),
                                ..Default::default()
                            });

                            draw_static_instances_system(
                                &gctx,
                                &map,
                                asset_manager,
                                &externs,
                                TfxRenderStage::Decals,
                            );

                            draw_dynamic_model_system(
                                &gctx,
                                &map,
                                asset_manager,
                                &externs,
                                TfxRenderStage::Decals,
                            );

                            tmp_gbuffers.rt0.copy_to(&tmp_gbuffers.staging_clone);
                            // tmp_gbuffers.rt0.copy_to(&tmp_gbuffers.staging);

                            unsafe {
                                gctx.context().OMSetRenderTargets(
                                    Some(&[
                                        Some(tmp_gbuffers.light_diffuse.render_target.clone()),
                                        Some(tmp_gbuffers.light_specular.render_target.clone()),
                                    ]),
                                    None,
                                );
                                gctx.context().ClearRenderTargetView(
                                    &tmp_gbuffers.light_diffuse.render_target,
                                    &[0.0, 0.0, 0.0, 0.0],
                                );
                                gctx.context().ClearRenderTargetView(
                                    &tmp_gbuffers.light_specular.render_target,
                                    &[0.0, 0.0, 0.0, 0.0],
                                );
                                gctx.context().ClearRenderTargetView(
                                    &tmp_gbuffers.staging.render_target,
                                    &[0.0, 0.0, 0.0, 0.0],
                                );
                            }

                            gctx.current_states.store(StateSelection::new(
                                Some(8),
                                Some(0),
                                Some(0),
                                Some(0),
                            ));

                            draw_light_system(&gctx, &map, asset_manager, camera, &mut externs);

                            unsafe {
                                gctx.context().OMSetRenderTargets(
                                    Some(&[Some(tmp_gbuffers.staging.render_target.clone()), None]),
                                    None,
                                );

                                gctx.context().OMSetDepthStencilState(None, 0);

                                let pipeline = &rglobals.pipelines.deferred_shading_no_atm;
                                if let Err(e) = pipeline.bind(gctx, &externs, asset_manager) {
                                    error!("Failed to run deferred_shading: {e}");
                                    return;
                                }

                                gctx.set_input_topology(EPrimitiveType::TriangleStrip);
                                gctx.context().Draw(6, 0);
                            }
                            unsafe {
                                gctx.context().OMSetRenderTargets(
                                    Some(&[Some(tmp_gbuffers.staging.render_target.clone()), None]),
                                    Some(&tmp_gbuffers.depth.view),
                                );
                                gctx.context()
                                    .OMSetDepthStencilState(&tmp_gbuffers.depth.state_readonly, 0);
                            }

                            rglobals
                                .scopes
                                .transparent
                                .bind(gctx, &asset_manager, &externs)
                                .unwrap();

                            gctx.current_states.store(StateSelection::new(
                                Some(2),
                                Some(15),
                                Some(2),
                                Some(1),
                            ));

                            draw_static_instances_system(
                                &gctx,
                                &map,
                                asset_manager,
                                &externs,
                                TfxRenderStage::DecalsAdditive,
                            );

                            draw_dynamic_model_system(
                                &gctx,
                                &map,
                                asset_manager,
                                &externs,
                                TfxRenderStage::DecalsAdditive,
                            );

                            draw_static_instances_system(
                                &gctx,
                                &map,
                                asset_manager,
                                &externs,
                                TfxRenderStage::Transparents,
                            );

                            draw_dynamic_model_system(
                                &gctx,
                                &map,
                                asset_manager,
                                &externs,
                                TfxRenderStage::Transparents,
                            );
                        }

                        unsafe {
                            gctx.context()
                                .OMSetRenderTargets(Some(&[None, None, None]), None);
                        }

                        gctx.blit_texture(
                            &tmp_gbuffers.staging.view,
                            // &tmp_gbuffers.light_specular.view,
                            gctx.swapchain_target.read().as_ref().unwrap(),
                        );

                        gui.draw_frame(window, |ctx, ectx| {
                            let mut gui_views = resources.get_mut::<GuiViewManager>();
                            gui_views.draw(ectx, window, resources, ctx);
                            puffin_egui::profiler_window(ectx);
                        });

                        gctx.present();

                        window.request_redraw();
                        profiling::finish_frame!();
                    }
                    _ => {}
                }
            }
        })?;

        Ok(())
    }
}

impl Drop for AlkahestApp {
    fn drop(&mut self) {
        config::persist();
    }
}