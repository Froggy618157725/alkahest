pub mod projection;
use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
pub use projection::CameraProjection;

pub mod fps;
pub mod orbit;
pub mod tween;

pub mod viewport;
pub use viewport::Viewport;

use self::{fps::FpsCamera, tween::Tween};
use crate::{
    input::InputState,
    tfx::view::{RenderStageSubscriptions, View},
};

pub trait CameraController {
    fn update(
        &mut self,
        tween: &mut Option<Tween>,
        input: &InputState,
        delta_time: f32,
        smooth_movement: bool,
    );
    fn update_mouse(&mut self, tween: &mut Option<Tween>, delta: Vec2, scroll: f32);

    // TODO(cohae): These might be a bit confusing
    /// Returns the position of the camera
    /// Orbit camera will return the target position instead
    fn position_target(&self) -> Vec3;

    /// Returns the position of the camera "view"
    /// Orbit camera will return the position of the view instead of the target
    fn position(&self) -> Vec3;

    fn rotation(&self) -> Quat;

    fn forward(&self) -> Vec3;
    fn right(&self) -> Vec3;
    fn up(&self) -> Vec3;

    fn view_matrix(&self) -> Mat4;

    fn set_position(&mut self, position: Vec3);
    // fn set_rotation(&mut self, rotation: Quat);
    // fn look_at(&mut self, target: Vec3);
}

pub struct Camera {
    controller: Box<dyn CameraController>,
    viewport: Viewport,

    pub projection: CameraProjection,
    pub tween: Option<Tween>,

    // Aka view matrix
    pub world_to_camera: Mat4,
    pub camera_to_world: Mat4,
    // Aka projection matrix
    pub camera_to_projective: Mat4,
    pub projective_to_camera: Mat4,
    // Aka view+projection matrix
    pub world_to_projective: Mat4,
    pub projective_to_world: Mat4,

    pub target_pixel_to_projective: Mat4,
}

impl Camera {
    pub fn new_fps(viewport: Viewport) -> Self {
        Self::new(
            viewport,
            CameraProjection::Perspective {
                fov: 90.0,
                near: 0.0001,
            },
            Box::<FpsCamera>::default(),
        )
    }

    pub fn new(
        viewport: Viewport,
        projection: CameraProjection,
        controller: Box<dyn CameraController>,
    ) -> Self {
        let mut camera = Self {
            controller,
            viewport,

            projection,
            tween: None,

            world_to_camera: Mat4::IDENTITY,
            camera_to_world: Mat4::IDENTITY,
            camera_to_projective: Mat4::IDENTITY,
            projective_to_camera: Mat4::IDENTITY,
            world_to_projective: Mat4::IDENTITY,
            projective_to_world: Mat4::IDENTITY,
            target_pixel_to_projective: Mat4::IDENTITY,
        };

        camera.update_matrices();
        camera
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = viewport;
    }

    pub fn set_projection(&mut self, projection: CameraProjection) {
        self.projection = projection;
    }

    pub fn update_mouse(&mut self, delta: Vec2, scroll: f32) {
        self.controller.update_mouse(&mut self.tween, delta, scroll);
        self.update_matrices();
    }

    pub fn update(&mut self, input: &InputState, delta_time: f32, smooth_movement: bool) {
        self.controller
            .update(&mut self.tween, input, delta_time, smooth_movement);
        self.update_matrices();
    }

    pub fn update_matrices(&mut self) {
        self.world_to_camera = self.controller.view_matrix();
        self.camera_to_world = self.world_to_camera.inverse();

        self.camera_to_projective = self.projection.matrix(self.viewport.aspect_ratio());
        self.projective_to_camera = self.camera_to_projective.inverse();

        self.world_to_projective = self.camera_to_projective * self.world_to_camera;
        self.projective_to_world = self.world_to_projective.inverse();

        self.target_pixel_to_projective = Mat4::from_cols_array_2d(&[
            [2.0 / self.viewport.size.x as f32, 0.0, 0.0, 0.0],
            [0.0, -2.0 / self.viewport.size.y as f32, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0, 1.0],
        ]);
    }
}

// Functions forwarded from CameraController
impl Camera {
    pub fn position_target(&self) -> Vec3 {
        self.controller.position_target()
    }

    pub fn position(&self) -> Vec3 {
        self.controller.position()
    }

    pub fn rotation(&self) -> glam::Quat {
        self.controller.rotation()
    }

    pub fn forward(&self) -> Vec3 {
        self.controller.forward()
    }

    pub fn right(&self) -> Vec3 {
        self.controller.right()
    }

    pub fn up(&self) -> Vec3 {
        self.controller.up()
    }

    pub fn set_position(&mut self, position: Vec3) {
        self.controller.set_position(position);
    }

    // pub fn set_rotation(&mut self, rotation: Quat) {
    //     self.controller.set_rotation(rotation);
    // }
    //
    // pub fn look_at(&mut self, target: Vec3) {
    //     self.controller.look_at(target);
    // }
}

impl View for Camera {
    fn get_viewport(&self) -> Viewport {
        self.viewport.clone()
    }

    fn get_subscribed_views(&self) -> RenderStageSubscriptions {
        RenderStageSubscriptions::all()
    }

    fn get_name(&self) -> String {
        "Camera".to_string()
    }

    fn update_extern(&self, x: &mut crate::tfx::externs::View) {
        x.resolution_width = self.viewport.size.x as f32;
        x.resolution_height = self.viewport.size.y as f32;
        x.camera_to_world = self.world_to_camera;
        x.world_to_projective = self.world_to_projective;
        x.position = self.controller.position().extend(1.0);
        x.unk30 = Vec4::Z - self.world_to_projective.w_axis;

        x.world_to_camera = self.world_to_camera;
        x.camera_to_projective = self.camera_to_projective;
        x.camera_to_world = self.camera_to_world;
        x.projective_to_camera = self.projective_to_camera;
        x.world_to_projective = self.world_to_projective;
        x.projective_to_world = self.projective_to_world;
        x.target_pixel_to_world = self.projective_to_world * self.target_pixel_to_projective;
        x.target_pixel_to_camera = self.projective_to_camera * self.target_pixel_to_projective;

        // // TODO(cohae): Still figuring out these transforms for lights
        // x.combined_tptoc_wtoc = x.target_pixel_to_camera;
        x.combined_tptoc_wtoc = x.target_pixel_to_world;
        // x.combined_tptoc_wtoc = x.world_to_camera * x.target_pixel_to_camera;

        // Only known values are (0, 1, 0, 0) and (0, 3.428143, 0, 0)
        x.view_miscellaneous = Vec4::new(0., 1., 0., 0.);
    }
}