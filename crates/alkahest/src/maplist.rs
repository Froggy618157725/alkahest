use alkahest_data::text::StringContainerShared;
use alkahest_renderer::{
    ecs::{
        common::Global,
        hierarchy::{Children, Parent},
        render::{
            dynamic_geometry::update_dynamic_model_system, light::update_shadowrenderer_system,
            static_geometry::update_static_instances_system,
        },
        resources::SelectedEntity,
        route::Route,
        visibility::propagate_entity_visibility_system,
        Scene, SceneInfo,
    },
    loaders::map::load_map,
    renderer::RendererShared,
    util::{
        scene::{EntityWorldMutExt, SceneExt},
        Hocus,
    },
};
use bevy_ecs::{
    entity::Entity,
    query::{With, Without},
    schedule::{ExecutorKind, Schedule, ScheduleLabel},
    system::Commands,
    world::CommandQueue,
};
use destiny_pkg::TagHash;
use itertools::Itertools;
use poll_promise::Promise;
use smallvec::SmallVec;

use crate::{
    discord, gui::activity_select::CurrentActivity, resources::AppResources, ApplicationArgs,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MapLoadState {
    #[default]
    Unloaded,
    Loading,
    Loaded,
    Error(String),
}

pub struct Map {
    pub hash: TagHash,
    pub name: String,
    pub load_promise: Option<Box<Promise<anyhow::Result<Scene>>>>,
    pub load_state: MapLoadState,

    pub command_queue: CommandQueue,
    pub scene: Scene,

    systems: Systems,
}

#[derive(ScheduleLabel, Debug, Hash, PartialEq, Eq, Clone)]
struct PreUpdate;

// TODO: Trash, fix and move to alkahest_renderer
struct Systems {
    /// Schedule ran before the main update
    pub(crate) schedule_pre: Schedule,
    pub(crate) schedule_pre_threadsafe: Schedule,
}

impl Systems {
    fn create(world: &mut Scene) -> Self {
        let mut schedule_pre = Schedule::new(PreUpdate);

        schedule_pre
            .add_systems((update_static_instances_system, update_dynamic_model_system))
            .set_executor_kind(ExecutorKind::SingleThreaded)
            .initialize(world)
            .unwrap();

        let mut schedule_pre_threadsafe = Schedule::new(PreUpdate);
        schedule_pre_threadsafe
            .add_systems((
                update_shadowrenderer_system,
                propagate_entity_visibility_system,
            ))
            .set_executor_kind(ExecutorKind::MultiThreaded)
            .initialize(world)
            .unwrap();

        Self {
            schedule_pre,
            schedule_pre_threadsafe,
        }
    }
}

impl Map {
    pub fn create_empty(name: impl AsRef<str>) -> Self {
        Self {
            load_state: MapLoadState::Loaded,
            ..Self::create(name, TagHash::NONE, None)
        }
    }

    pub fn create(name: impl AsRef<str>, hash: TagHash, activity_hash: Option<TagHash>) -> Self {
        let mut scene = Scene::new_with_info(activity_hash, hash);

        Self {
            hash,
            name: name.as_ref().to_string(),
            load_promise: Default::default(),
            load_state: Default::default(),

            systems: Systems::create(&mut scene),
            scene,
            command_queue: Default::default(),
        }
    }

    pub(super) fn update_load(&mut self) {
        if let Some(promise) = self.load_promise.take() {
            if promise.ready().is_some() {
                match promise.block_and_take() {
                    Ok(mut scene) => {
                        // Move all globals to a temporary scene
                        std::mem::swap(&mut self.scene, &mut scene);
                        self.systems = Systems::create(&mut self.scene);
                        self.take_globals(&mut scene);

                        info!(
                            "Loaded map {} with {} entities",
                            self.name,
                            self.scene.entities().len()
                        );

                        self.load_state = MapLoadState::Loaded;
                    }
                    Err(e) => {
                        error!("Failed to load map {} '{}': {:?}", self.hash, self.name, e);
                        self.load_state = MapLoadState::Error(format!("{:?}", e));
                    }
                }
            } else {
                self.load_promise = Some(promise);
                self.load_state = MapLoadState::Loading;
            }
        }
    }

    pub fn update(&mut self) {
        self.command_queue.apply(&mut self.scene);
        self.scene.clear_trackers();
        self.scene.check_change_ticks();

        self.systems.schedule_pre.run(&mut self.scene);
        self.systems.schedule_pre_threadsafe.run(&mut self.scene);
    }

    /// Remove global entities from the scene and store them in this one
    pub fn take_globals(&mut self, source: &mut Scene) {
        let ent_list = source
            .query_filtered::<Entity, (With<Global>, Without<Parent>)>()
            .iter(source)
            .collect_vec();
        let mut new_selected_entity: Option<Entity> = None;

        {
            // TODO(cohae): selected_entity always appears to be None, and thus the selected entity isn't carried over
            let selected_entity = source.resource::<SelectedEntity>().selected();
            for entity in ent_list {
                let old_entity_components = source.take_boxed(entity).unwrap();
                let new_entity = self.scene.spawn_boxed(old_entity_components);

                if new_selected_entity.is_none() && selected_entity == Some(entity) {
                    new_selected_entity = Some(new_entity);
                }

                let Some(children) = self.scene.entity_mut(new_entity).take::<Children>() else {
                    continue;
                };
                self.fixup_children(
                    source,
                    new_entity,
                    &children,
                    &mut new_selected_entity,
                    &selected_entity,
                );
            }
        }

        if let Some(new_entity) = new_selected_entity {
            self.scene
                .resource_mut::<SelectedEntity>()
                .select(new_entity);
        }
    }

    fn fixup_children(
        &mut self,
        source: &mut Scene,
        new_parent: Entity,
        children: &Children,
        new_selected: &mut Option<Entity>,
        selected: &Option<Entity>,
    ) {
        let mut new_children = Children(SmallVec::new());
        for child in children.0.iter() {
            let Some(old_entity_components) = source.take_boxed(*child) else {
                continue;
            };
            let new_entity = self.scene.spawn_boxed(old_entity_components);
            if new_selected.is_none() && selected.is_some_and(|e| e == *child) {
                new_selected.replace(new_entity);
            }
            new_children.0.push(new_entity);
            if let Some(mut parent) = self.scene.entity_mut(new_entity).get_mut::<Parent>() {
                parent.0 = new_parent;
            }

            let Some(grandchildren) = self.scene.entity_mut(new_entity).take::<Children>() else {
                continue;
            };
            self.fixup_children(source, new_entity, &grandchildren, new_selected, selected);
        }
        self.scene.entity_mut(new_parent).insert_one(new_children);
    }

    fn fixup_route_visibility(&mut self) {
        for (e, r) in self.scene.query::<(Entity, &Route)>().iter(&self.scene) {
            r.fixup_visiblity(&self.scene, &mut self.commands(), e);
        }
    }

    fn start_load(&mut self, resources: &AppResources) {
        if self.load_state != MapLoadState::Unloaded {
            warn!(
                "Attempted to load map {}, but it is already loading or loaded",
                self.hash
            );
            return;
        }

        let renderer = resources.get::<RendererShared>().clone();
        let cli_args = resources.get::<ApplicationArgs>();
        let activity_hash = resources.get_mut::<CurrentActivity>().0;
        let global_strings = resources.get::<StringContainerShared>().clone();

        info!("Loading map {} '{}'", self.hash, self.name);
        self.load_promise = Some(Box::new(Promise::spawn_async(load_map(
            renderer,
            self.hash,
            activity_hash,
            global_strings,
            !cli_args.no_ambient,
        ))));

        self.load_state = MapLoadState::Loading;
    }

    pub fn commands(&self) -> Commands<'_, '_> {
        Commands::new(&mut self.pocus().command_queue, &self.scene)
    }
}

#[derive(Default)]
pub struct MapList {
    current_map: usize,
    pub previous_map: Option<usize>,

    pub load_all_maps: bool,

    pub maps: Vec<Map>,
}

impl MapList {
    pub fn current_map_index(&self) -> usize {
        self.current_map
    }

    pub fn current_map(&self) -> Option<&Map> {
        self.maps.get(self.current_map)
    }

    pub fn current_map_mut(&mut self) -> Option<&mut Map> {
        self.maps.get_mut(self.current_map)
    }

    // pub fn get_map_mut(&mut self, index: usize) -> Option<&mut Map> {
    //     self.maps.get_mut(index)
    // }

    pub fn count_loading(&self) -> usize {
        self.maps
            .iter()
            .filter(|m| m.load_state == MapLoadState::Loading)
            .count()
    }

    pub fn count_loaded(&self) -> usize {
        self.maps
            .iter()
            .filter(|m| m.load_state == MapLoadState::Loaded)
            .count()
    }
}

impl MapList {
    pub fn update_maps(&mut self, resources: &AppResources) {
        for (i, map) in self.maps.iter_mut().enumerate() {
            map.update_load();
            if i == self.current_map && map.load_state == MapLoadState::Unloaded {
                map.start_load(resources);
            }
        }

        if self.load_all_maps {
            const LOAD_MAX_PARALLEL: usize = 4;
            let mut loaded = 0;
            for map in self.maps.iter_mut() {
                if loaded >= LOAD_MAX_PARALLEL {
                    break;
                }

                if map.load_state == MapLoadState::Loading {
                    loaded += 1;
                }

                if map.load_state == MapLoadState::Unloaded {
                    map.start_load(resources);
                    loaded += 1;
                }
            }
        }
    }

    /// Populates the map list and begins loading the first map
    /// Overwrites the current map list
    pub fn set_maps(&mut self, resources: &AppResources, map_hashes: &[(TagHash, String)]) {
        let activity_hash = resources.get_mut::<CurrentActivity>().0;
        self.maps = map_hashes
            .iter()
            .map(|(hash, name)| Map::create(name, *hash, activity_hash))
            .collect();

        #[cfg(not(feature = "keep_map_order"))]
        self.maps.sort_by_key(|m| m.name.clone());

        self.current_map = 0;
        self.previous_map = None;

        #[cfg(feature = "discord_rpc")]
        if let Some(map) = self.current_map() {
            discord::set_activity_from_map(map);
        }
    }

    pub fn add_map(&mut self, resources: &AppResources, map_name: String, map_hash: TagHash) {
        if self.maps.is_empty() {
            self.set_maps(resources, &[(map_hash, map_name.clone())])
        } else {
            let activity_hash = resources.get_mut::<CurrentActivity>().0;
            self.maps
                .push(Map::create(map_name, map_hash, activity_hash))
        }
    }

    pub fn set_current_map(&mut self, index: usize) {
        if index >= self.maps.len() {
            warn!(
                "Attempted to set current map to index {}, but there are only {} maps",
                index,
                self.maps.len()
            );
            return;
        }

        self.previous_map = Some(self.current_map);
        self.current_map = index;

        if let Some(previous_map) = self.previous_map {
            if previous_map >= self.maps.len() {
                warn!(
                    "Previous map index {} is out of bounds, not migrating globals",
                    previous_map
                );
                self.previous_map = None;
                return;
            }

            let mut source = std::mem::take(&mut self.maps[previous_map].scene);
            let dest = &mut self.maps[self.current_map];
            dest.take_globals(&mut source);
            dest.fixup_route_visibility();
            self.maps[previous_map].scene = source;
        }

        #[cfg(feature = "discord_rpc")]
        if let Some(map) = self.current_map() {
            discord::set_activity_from_map(map);
        }
    }

    pub fn set_current_map_next(&mut self) {
        if self.current_map + 1 < self.maps.len() {
            self.set_current_map(self.current_map + 1)
        }
    }

    pub fn set_current_map_prev(&mut self) {
        if self.current_map > 0 && !self.maps.is_empty() {
            self.set_current_map(self.current_map - 1)
        }
    }
}
