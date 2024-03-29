pub mod component_panels;
pub mod components;
pub mod resources;
pub mod tags;
pub mod transform;

pub type Scene = hecs::World;

use alkahest_data::occlusion::AABB;
use glam::Vec3;
use hecs::EntityRef;
use itertools::Itertools;

use self::transform::Transform;
use crate::{
    ecs::{component_panels::ComponentPanel, components::*},
    util::text::split_pascal_case,
};

pub fn resolve_entity_icon(e: EntityRef<'_>) -> Option<char> {
    macro_rules! icon_from_component_panels {
		($($component:ty),+) => {
			$(
				if e.has::<$component>() {
					return Some(<$component>::inspector_icon());
				}
			)*
		};
	}

    if let Some(rp) = e.get::<&ResourcePoint>() {
        return Some(rp.resource.debug_icon());
    }

    icon_from_component_panels!(
        // TODO(cohae): Custom havok icon
        // HavokShape,
        Beacon,
        Ruler,
        Route,
        Sphere,
        EntityModel,
        StaticInstances,
        Light
    );

    None
}

pub fn resolve_entity_name(e: EntityRef<'_>, append_ent: bool) -> String {
    let postfix = if append_ent {
        format!(" (ent {})", e.entity().id())
    } else {
        String::new()
    };

    if let Some(label) = e.get::<&Label>() {
        format!("{}{postfix}", label.0)
    } else if let Some(rp) = e.get::<&ResourcePoint>() {
        format!("{}{postfix}", split_pascal_case(rp.resource.debug_id()))
    } else {
        macro_rules! name_from_component_panels {
            ($($component:ty),+) => {
                $(
                    if e.has::<$component>() {
                        return format!("{}{postfix}", <$component>::inspector_name());
                    }
                )*
            };
        }

        name_from_component_panels!(Beacon, Route, Ruler, Sphere, EntityModel, StaticInstances);

        format!("ent {}", e.entity().id())
    }
}

pub fn resolve_aabb(e: EntityRef<'_>) -> Option<AABB> {
    if let Some(ruler) = e.get::<&Ruler>() {
        return Some(AABB::from_points([ruler.start, ruler.end]));
    }

    if let Some(si) = e.get::<&StaticInstances>() {
        let points =
            si.0.occlusion_bounds
                .iter()
                .flat_map(|v| [v.min, v.max])
                .collect_vec();
        return Some(AABB::from_points(points));
    }

    if let Some(transform) = e.get::<&Transform>() {
        let radius = transform.radius();
        if radius.is_normal() {
            return Some(AABB::from_points([
                transform.translation - Vec3::ONE * radius,
                transform.translation + Vec3::ONE * radius,
            ]));
        }
    }

    None
}
