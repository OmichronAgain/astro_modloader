use std::{
    collections::HashMap,
    io::{self, ErrorKind},
    path::Path,
};

use lazy_static::lazy_static;

use regex::Regex;
use unreal_asset::{
    cast,
    exports::Export,
    flags::EObjectFlags,
    properties::{
        array_property::ArrayProperty, enum_property::EnumProperty, guid_property::GuidProperty,
        int_property::BoolProperty, object_property::{ObjectProperty, SoftObjectProperty}, str_property::NameProperty,
        struct_property::StructProperty, Property, PropertyDataTrait,
    },
    ue4version::VER_UE4_23,
    unreal_types::{FName, PackageIndex},
    Asset, Import,
};
use unreal_modintegrator::{find_asset, read_asset, write_asset, IntegratorConfig};
use unreal_pak::PakFile;
use uuid::Uuid;

use crate::assets::{ACTOR_TEMPLATE_ASSET, LEVEL_TEMPLATE_ASSET};

pub struct AstroIntegratorConfig;

fn get_asset(
    integrated_pak: &mut PakFile,
    game_paks: &mut Vec<PakFile>,
    name: &String,
    version: i32,
) -> Result<Asset, io::Error> {
    if let Ok(asset) = read_asset(integrated_pak, version, name) {
        return Ok(asset);
    }
    let original_asset =
        find_asset(game_paks, name).ok_or(io::Error::new(ErrorKind::Other, "No such ass"))?;

    read_asset(&mut game_paks[original_asset], version, name)
        .map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))
}

static MAP_PATHS: [&'static str; 3] = [
    "Astro/Content/Maps/Staging_T2.umap",
    "Astro/Content/Maps/Staging_T2_PackedPlanets_Switch.umap",
    //"Astro/Content/Maps/TutorialMoon_Prototype_v2.umap", // Tutorial not integrated for performance
    "Astro/Content/Maps/test/BasicSphereT2.umap",
];

lazy_static! {
    static ref GAME_REGEX: Regex = Regex::new("^/Game/").expect("Failed to compile GAME_REGEX");
}

fn game_to_absolute(path: &str) -> Option<String> {
    if !GAME_REGEX.is_match(path) {
        return None;
    }

    let path_str = GAME_REGEX.replace(path, "Astro/Content/").to_string();
    let path = Path::new(&path_str);
    match path.extension() {
        Some(_) => Some(path_str),
        None => path
            .with_extension("uasset")
            .to_str()
            .map(|e| e.to_string()),
    }
}

fn handle_mission_trailheads(
    _data: &(),
    integrated_pak: &mut PakFile,
    game_paks: &mut Vec<PakFile>,
    trailhead_arrays: Vec<&serde_json::Value>,
) -> Result<(), io::Error> {
    for map_path in MAP_PATHS {
        let mut asset = get_asset(
            integrated_pak,
            game_paks,
            &String::from(map_path),
            VER_UE4_23,
        )?;

        let mut additional_properties: Vec<Property> = Vec::new();

        for trailheads in &trailhead_arrays {
            let trailheads = trailheads
                .as_array()
                .ok_or(io::Error::new(ErrorKind::Other, "Invalid trailheads"))?;
            for trailhead in trailheads {
                asset.add_name_reference(String::from("AstroMissionDataAsset"), false);

                let trailhead = trailhead
                    .as_str()
                    .ok_or(io::Error::new(ErrorKind::Other, "Invalid trailheads"))?;
                let soft_class_name = Path::new(trailhead)
                    .file_stem()
                    .map(|e| e.to_str())
                    .flatten()
                    .ok_or(io::Error::new(ErrorKind::Other, "Invalid trailheads"))?;

                asset.add_name_reference(String::from(trailhead), false);
                asset.add_name_reference(String::from(soft_class_name), false);

                let package_import = Import {
                    class_package: FName::from_slice("/Script/CoreUObject"),
                    class_name: FName::from_slice("Package"),
                    outer_index: PackageIndex::new(0),
                    object_name: FName::from_slice(trailhead),
                };
                let package_import = asset.add_import(package_import);

                let new_import = Import {
                    class_package: FName::from_slice("/Script/Astro"),
                    class_name: FName::from_slice("AstroMissionDataAsset"),
                    outer_index: package_import,
                    object_name: FName::from_slice(soft_class_name),
                };
                let new_import = asset.add_import(new_import);

                additional_properties.push(
                    ObjectProperty {
                        name: FName::from_slice("dummy"),
                        property_guid: None,
                        duplication_index: 0,
                        value: new_import,
                    }
                    .into(),
                );
            }
        }

        let mut export_index = None;

        for i in 0..asset.exports.len() {
            match &asset.exports[i] {
                Export::NormalExport(e) => {
                    if e.base_export.class_index.is_import() {
                        if asset
                            .get_import(e.base_export.class_index)
                            .map(|e| &e.object_name.content == "AstroSettings")
                            .unwrap_or(false)
                        {
                            export_index = Some(i);
                            break;
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(export_index) = export_index {
            let export = &mut asset.exports[export_index];
            match export {
                Export::NormalExport(export) => {
                    additional_properties.iter_mut().for_each(|e| match e {
                        Property::ObjectProperty(e) => {
                            e.name = export.base_export.object_name.clone()
                        }
                        _ => panic!("Corrupted memory"),
                    });
                    export.properties.extend(additional_properties);
                }
                _ => {}
            }
        }

        write_asset(integrated_pak, &asset, &String::from(map_path))
            .map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;
    }

    Ok(())
}

#[derive(Default)]
struct ScsNode {
    internal_variable_name: String,
    type_link: PackageIndex,
    attach_parent: Option<PackageIndex>,
    original_category: PackageIndex,
}

fn handle_persistent_actors(
    _data: &(),
    integrated_pak: &mut PakFile,
    game_paks: &mut Vec<PakFile>,
    persistent_actor_arrays: Vec<&serde_json::Value>,
) -> Result<(), io::Error> {
    let mut level_asset = Asset::new(LEVEL_TEMPLATE_ASSET.to_vec(), None);
    level_asset.engine_version = VER_UE4_23;
    level_asset
        .parse_data()
        .map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;

    let actor_template = level_asset
        .get_export(PackageIndex::new(2))
        .map(|e| match e {
            Export::NormalExport(e) => Some(e),
            _ => None,
        })
        .flatten()
        .ok_or(io::Error::new(ErrorKind::Other, "Corrupted actor_template"))?;

    let scene_component = level_asset
        .get_export(PackageIndex::new(11))
        .map(|e| match e {
            Export::NormalExport(e) => Some(e),
            _ => None,
        })
        .flatten()
        .ok_or(io::Error::new(
            ErrorKind::Other,
            "Corrupted scene_component",
        ))?;

    for map_path in MAP_PATHS {
        let mut asset = get_asset(
            integrated_pak,
            game_paks,
            &String::from(map_path),
            VER_UE4_23,
        )?;

        let mut level_index = None;
        for i in 0..asset.exports.len() {
            let export = &asset.exports[i];
            match export {
                Export::LevelExport(_) => {
                    level_index = Some(i);
                    break;
                }
                _ => {}
            }
        }
        if level_index.is_none() {
            return Err(io::Error::new(
                ErrorKind::Other,
                "Unable to find Level category",
            ));
        }
        let level_index = level_index.unwrap();

        asset.add_fname("bHidden");
        asset.add_fname("bNetAddressable");
        asset.add_fname("CreationMethod");
        asset.add_fname("EComponentCreationMethod");
        asset.add_fname("EComponentCreationMethod::SimpleConstructionScript");
        asset.add_fname("BlueprintCreatedComponents");
        asset.add_fname("AttachParent");
        asset.add_fname("RootComponent");

        for persistent_actors in &persistent_actor_arrays {
            let persistent_actors = persistent_actors.as_array().ok_or(io::Error::new(
                ErrorKind::Other,
                "Invalid persistent actors",
            ))?;

            for persistent_actor in persistent_actors {
                let actor_path_raw = persistent_actor.as_str().ok_or(io::Error::new(
                    ErrorKind::Other,
                    "Invalid persistent actors",
                ))?;
                let actor = Path::new(actor_path_raw)
                    .file_stem()
                    .map(|e| e.to_str())
                    .flatten()
                    .ok_or(io::Error::new(
                        ErrorKind::Other,
                        "Invalid persistent actors",
                    ))?;

                let (actor_path_raw, actor) = match actor.contains(".") {
                    true => {
                        let split: Vec<&str> = actor.split(".").collect();
                        (split[0], &split[1][..split[1].len() - 2])
                    }
                    false => (actor_path_raw, actor),
                };

                asset.add_fname(actor_path_raw);
                asset.add_fname(&(String::from(actor) + "_C"));
                asset.add_fname(&(String::from("Default__") + actor + "_C"));
                asset.add_fname(actor);

                let first_import = Import {
                    class_package: FName::from_slice("/Script/CoreUObject"),
                    class_name: FName::from_slice("Package"),
                    outer_index: PackageIndex::new(0),
                    object_name: FName::from_slice(actor_path_raw),
                };
                let first_import = asset.add_import(first_import);

                let blueprint_import = Import {
                    class_package: FName::from_slice("/Script/Engine"),
                    class_name: FName::from_slice("BlueprintGeneratedClass"),
                    outer_index: first_import,
                    object_name: FName::new(String::from(actor) + "_C", 0),
                };
                let blueprint_import = asset.add_import(blueprint_import);

                let component_import = Import {
                    class_package: FName::from_slice(actor_path_raw),
                    class_name: FName::new(String::from(actor) + "_C", 0),
                    outer_index: blueprint_import,
                    object_name: FName::new(String::from("Default__") + actor + "_C", 0),
                };
                let component_import = asset.add_import(component_import);

                let mut actor_template = actor_template.clone();
                actor_template.base_export.class_index = blueprint_import;
                actor_template.base_export.object_name = FName::from_slice(actor);
                actor_template.base_export.template_index = component_import;
                actor_template.base_export.outer_index = PackageIndex::new(level_index as i32 + 1);

                let mut all_blueprint_created_components = Vec::new();

                let asset_name = game_to_absolute(actor).ok_or(io::Error::new(
                    ErrorKind::Other,
                    "Invalid persistent actor path",
                ))?;

                let game_asset = find_asset(game_paks, &asset_name)
                    .map(|e| read_asset(&mut game_paks[e], VER_UE4_23, &asset_name).ok())
                    .flatten();
                if let Some(game_asset) = game_asset {
                    let mut scs_export = None;

                    for i in 0..game_asset.exports.len() {
                        let export = &game_asset.exports[i];
                        match export {
                            Export::NormalExport(normal_export) => {
                                if normal_export.base_export.class_index.is_import() {
                                    let is_scs = game_asset
                                        .get_import(normal_export.base_export.class_index)
                                        .map(|e| {
                                            &e.object_name.content == "SimpleConstructionScript"
                                        })
                                        .unwrap_or(false);

                                    if is_scs {
                                        scs_export = Some(normal_export);
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    if let Some(scs_export) = scs_export {
                        let mut known_node_categories = Vec::new();

                        for property in &scs_export.properties {
                            if let Some(array_property) = cast!(Property, ArrayProperty, property) {
                                if array_property
                                    .array_type
                                    .as_ref()
                                    .map(|e| e.content == "ObjectProperty")
                                    .unwrap_or(false)
                                    && array_property.name.content == "AllNodes"
                                {
                                    for array_element in &array_property.value {
                                        if let Some(object_property) =
                                            cast!(Property, ObjectProperty, array_element)
                                        {
                                            if object_property.value.index > 0 {
                                                known_node_categories.push(object_property.value);
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let mut known_parents = HashMap::new();
                        for known_node_category in known_node_categories {
                            let known_category =
                                &game_asset.exports[known_node_category.index as usize - 1];
                            if let Some(known_normal_category) =
                                cast!(Export, NormalExport, known_category)
                            {
                                let is_scs_node = match known_normal_category
                                    .base_export
                                    .class_index
                                    .is_import()
                                {
                                    true => game_asset
                                        .get_import(known_normal_category.base_export.class_index)
                                        .map(|e| e.object_name.content == "SCS_Node")
                                        .unwrap_or(false),
                                    false => false,
                                };
                                if !is_scs_node {
                                    continue;
                                }

                                let mut new_scs = ScsNode::default();
                                new_scs.internal_variable_name = String::from("Unknown");
                                new_scs.original_category = known_node_category;

                                let mut import_1 = None;
                                let mut import_2 = None;

                                for property in &known_normal_category.properties {
                                    match property.get_name().content.as_str() {
                                        "InternalVariableName" => {
                                            if let Some(name_property) =
                                                cast!(Property, NameProperty, property)
                                            {
                                                new_scs.internal_variable_name =
                                                    name_property.value.content.clone();
                                            }
                                        }
                                        "ComponentClass" => {
                                            if let Some(object_property) =
                                                cast!(Property, ObjectProperty, property)
                                            {
                                                let import = game_asset
                                                    .get_import(object_property.value)
                                                    .ok_or(io::Error::new(
                                                        ErrorKind::Other,
                                                        "No such link",
                                                    ))?;
                                                import_1 = Some(import);
                                                import_2 = Some(
                                                    game_asset
                                                        .get_import(import.outer_index)
                                                        .ok_or(io::Error::new(
                                                            ErrorKind::Other,
                                                            "No such link",
                                                        ))?,
                                                );
                                            }
                                        }
                                        "ChildNodes" => {
                                            if let Some(array_property) =
                                                cast!(Property, ArrayProperty, property)
                                            {
                                                if array_property
                                                    .array_type
                                                    .as_ref()
                                                    .map(|e| e.content == "ObjectProperty")
                                                    .unwrap_or(false)
                                                {
                                                    for property in &array_property.value {
                                                        if let Some(object_property) = cast!(
                                                            Property,
                                                            ObjectProperty,
                                                            property
                                                        ) {
                                                            known_parents.insert(
                                                                object_property.value,
                                                                known_node_category,
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }

                                if let (Some(import_1), Some(import_2)) = (import_1, import_2) {
                                    let added_import = asset.find_import(
                                        &import_2.class_package,
                                        &import_2.class_name,
                                        import_2.outer_index,
                                        &import_2.object_name,
                                    );
                                    if let Some(added_import) = added_import {
                                        asset.add_import(import_2.clone());
                                    }

                                    let new_type_import = asset.find_import(
                                        &import_1.class_package,
                                        &import_1.class_name,
                                        import_1.outer_index,
                                        &import_1.object_name,
                                    );
                                    let new_type_import = match new_type_import {
                                        Some(_) => asset.add_import(import_1.clone()),
                                        None => PackageIndex::new(0),
                                    };
                                    new_scs.type_link = new_type_import;
                                }

                                all_blueprint_created_components.push(new_scs);
                            }
                        }

                        for node in &mut all_blueprint_created_components {
                            if let Some(parent) = known_parents.get(&node.original_category) {
                                node.attach_parent = Some(*parent);
                            }
                        }
                    }
                }

                let template_category_pointer =
                    (asset.exports.len() + all_blueprint_created_components.len() + 1) as i32;

                let mut serialized_blueprint_created_components: Vec<Property> = Vec::new();
                let mut scene_exports: Vec<Export> = Vec::new();

                let mut node_name_to_export_index = HashMap::new();
                let mut old_export_to_new_export = HashMap::new();

                for blueprint_created_component in &all_blueprint_created_components {
                    let mut scene_export = scene_component.clone();

                    scene_export.base_export.class_index = blueprint_created_component.type_link;
                    asset.add_name_reference(
                        blueprint_created_component.internal_variable_name.clone(),
                        false,
                    );
                    scene_export.base_export.object_name = FName::new(
                        blueprint_created_component.internal_variable_name.clone(),
                        0,
                    );
                    scene_export.base_export.outer_index =
                        PackageIndex::new(template_category_pointer);

                    let mut prop_data: Vec<Property> = Vec::from([
                        BoolProperty {
                            name: FName::from_slice("bNetAddressable"),
                            property_guid: None,
                            duplication_index: 0,
                            value: true,
                        }
                        .into(),
                        EnumProperty {
                            name: FName::from_slice("CreationMethod"),
                            property_guid: None,
                            duplication_index: 0,
                            enum_type: Some(FName::from_slice("EComponentCreationMethod")),
                            value: FName::from_slice(
                                "EComponentCreationMode::SimpleConstructionScript",
                            ),
                        }
                        .into(),
                    ]);

                    if let Some(attach_parent) = blueprint_created_component.attach_parent {
                        let next_parent = ObjectProperty {
                            name: FName::from_slice("AttachParent"),
                            property_guid: None,
                            duplication_index: 0,
                            value: attach_parent,
                        };
                        prop_data.push(next_parent.into());
                    }

                    scene_export.extras = [0u8; 4].to_vec();
                    scene_export.properties = prop_data;
                    scene_exports.push(scene_export.into());

                    let count = (asset.exports.len() + scene_exports.len()) as i32;
                    serialized_blueprint_created_components.push(
                        ObjectProperty {
                            name: FName::from_slice("BlueprintCreatedComponents"),
                            property_guid: None,
                            duplication_index: 0,
                            value: PackageIndex::new(count),
                        }
                        .into(),
                    );
                    node_name_to_export_index.insert(
                        blueprint_created_component.internal_variable_name.clone(),
                        count,
                    );
                    old_export_to_new_export
                        .insert(blueprint_created_component.original_category, count);

                    let import = Import {
                        class_package: FName::from_slice("/Script/Engine"),
                        class_name: asset
                            .get_import(blueprint_created_component.type_link)
                            .ok_or(io::Error::new(ErrorKind::Other, "No such import"))?
                            .object_name
                            .clone(),
                        outer_index: actor_template.base_export.class_index,
                        object_name: FName::new(
                            String::from(
                                blueprint_created_component.internal_variable_name.clone(),
                            ) + "_GEN_VARIABLE",
                            0,
                        ),
                    };
                    asset.add_import(import);
                }

                for export in &mut scene_exports {
                    if let Some(normal_export) = cast!(Export, NormalExport, export) {
                        for property in &mut normal_export.properties {
                            if let Some(object_property) = cast!(Property, ObjectProperty, property)
                            {
                                if object_property.name.content == "AttachParent" {
                                    object_property.value = PackageIndex::new(
                                        old_export_to_new_export[&object_property.value],
                                    );
                                }
                            }
                        }
                    }
                }

                asset.exports.extend(scene_exports);

                let mut template_prop_data: Vec<Property> = Vec::from([BoolProperty {
                    name: FName::from_slice("bHidden"),
                    property_guid: None,
                    duplication_index: 0,
                    value: true,
                }
                .into()]);

                let mut array_template_prop = ArrayProperty::default();
                array_template_prop.name = FName::from_slice("BlueprintCreatedComponents");
                array_template_prop.array_type = Some(FName::from_slice("ObjectProperty"));
                array_template_prop.value = serialized_blueprint_created_components;
                template_prop_data.push(array_template_prop.into());

                for (node_name, export_index) in node_name_to_export_index {
                    if node_name == "DefaultSceneRoot" {
                        template_prop_data.push(
                            ObjectProperty {
                                name: FName::from_slice("RootComponent"),
                                property_guid: None,
                                duplication_index: 0,
                                value: PackageIndex::new(export_index),
                            }
                            .into(),
                        );
                    }
                    template_prop_data.push(
                        ObjectProperty {
                            name: FName::new(node_name, 0),
                            property_guid: None,
                            duplication_index: 0,
                            value: PackageIndex::new(export_index),
                        }
                        .into(),
                    );
                }

                actor_template
                    .base_export
                    .serialization_before_create_dependencies
                    .push(blueprint_import);
                actor_template
                    .base_export
                    .serialization_before_create_dependencies
                    .push(component_import);
                actor_template
                    .base_export
                    .create_before_create_dependencies
                    .push(PackageIndex::new(level_index as i32 + 1));
                actor_template.extras = [0u8; 4].to_vec();
                actor_template.properties = template_prop_data;
                asset.exports.push(actor_template.into());

                let len = asset.exports.len() as i32;
                let level_export = &mut asset.exports[level_index];
                let level_export =
                    cast!(Export, LevelExport, level_export).expect("Corrupted memory");
                level_export.index_data.push(len);
                level_export
                    .normal_export
                    .base_export
                    .create_before_serialization_dependencies
                    .push(PackageIndex::new(len));
            }
        }

        write_asset(integrated_pak, &asset, &String::from(map_path))
            .map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;
    }
    Ok(())
}

fn handle_linked_actor_components(
    _data: &(),
    integrated_pak: &mut PakFile,
    game_paks: &mut Vec<PakFile>,
    linked_actors_maps: Vec<&serde_json::Value>,
) -> Result<(), io::Error> {
    let mut actor_asset = Asset::new(ACTOR_TEMPLATE_ASSET.to_vec(), None);
    actor_asset.engine_version = VER_UE4_23;
    actor_asset
        .parse_data()
        .map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;

    let object_property_template = actor_asset
        .exports
        .get(6)
        .map(|e| cast!(Export, RawExport, e))
        .flatten()
        .ok_or(io::Error::new(ErrorKind::Other, "Corrupted LevelTemplate"))?;

    let template_export = actor_asset
        .exports
        .get(5)
        .map(|e| cast!(Export, NormalExport, e))
        .flatten()
        .ok_or(io::Error::new(ErrorKind::Other, "Corrupted LevelTemplate"))?;

    let scs_node_template = actor_asset
        .exports
        .get(10)
        .map(|e| cast!(Export, NormalExport, e))
        .flatten()
        .ok_or(io::Error::new(ErrorKind::Other, "Corrupted LevelTemplate"))?;

    let mut new_components = HashMap::new();

    for linked_actor_map in &linked_actors_maps {
        let linked_actors_map = linked_actor_map.as_object().ok_or(io::Error::new(
            ErrorKind::Other,
            "Invalid linked_actor_components",
        ))?;
        for (name, components) in linked_actors_map.iter() {
            let components = components.as_array().ok_or(io::Error::new(
                ErrorKind::Other,
                "Invalid linked_actor_components",
            ))?;

            let entry = new_components
                .entry(name.clone())
                .or_insert_with(|| Vec::new());
            for component in components {
                let component_name = component.as_str().ok_or(io::Error::new(
                    ErrorKind::Other,
                    "Invalid linked_actor_components",
                ))?;
                entry.push(String::from(component_name));
            }
        }
    }

    for (name, components) in &new_components {
        let name = game_to_absolute(&name)
            .ok_or(io::Error::new(ErrorKind::Other, "Invalid asset name"))?;
        let mut asset = get_asset(integrated_pak, game_paks, &name, VER_UE4_23)?;

        let mut scs_location = None;
        let mut bgc_location = None;
        let mut cdo_location = None;
        let mut node_offset = 0;

        for i in 0..asset.exports.len() {
            let export = &asset.exports[i];
            if let Some(normal_export) = cast!(Export, NormalExport, export) {
                let name = match normal_export.base_export.class_index.is_import() {
                    true => {
                        let import = asset
                            .get_import(normal_export.base_export.class_index)
                            .ok_or(io::Error::new(ErrorKind::Other, "No such import"))?;
                        import.class_name.content.clone()
                    }
                    false => String::new(),
                };

                match name.as_str() {
                    "SimpleConstructionScript" => scs_location = Some(i),
                    "BlueprintGeneratedClass" => bgc_location = Some(i),
                    "SCS_Node" => node_offset += 0,
                    _ => {}
                };
                if (EObjectFlags::RF_CLASS_DEFAULT_OBJECT
                    & EObjectFlags::from_bits(normal_export.base_export.object_flags)
                        .ok_or(io::Error::new(ErrorKind::Other, "Invalid export"))?)
                    == EObjectFlags::RF_CLASS_DEFAULT_OBJECT
                {
                    cdo_location = Some(i);
                }
            }
        }

        let (scs_location, bgc_location, cdo_location) = {
            (
                scs_location.ok_or(io::Error::new(
                    ErrorKind::Other,
                    "Unable to find SimpleConstructionScript",
                ))? as i32,
                bgc_location.ok_or(io::Error::new(
                    ErrorKind::Other,
                    "Unable to find BlueprintGeneratedClass",
                ))? as i32,
                cdo_location.ok_or(io::Error::new(ErrorKind::Other, "Unable to find CDO"))? as i32,
            )
        };

        let object_property_import = asset
            .find_import_no_index(
                &FName::from_slice("/Script/CoreUObject"),
                &FName::from_slice("Clawss"),
                &FName::from_slice("ObjectProperty"),
            )
            .ok_or(io::Error::new(ErrorKind::Other, "No such import"))?;
        let default_object_property_import = asset
            .find_import_no_index(
                &FName::from_slice("/Script/CoreUObject"),
                &FName::from_slice("ObjectProperty"),
                &FName::from_slice("Default__ObjectProperty"),
            )
            .ok_or(io::Error::new(ErrorKind::Other, "No such import"))?;

        let scs_node_import = asset
            .find_import_no_index(
                &FName::from_slice("/Script/CoreUObject"),
                &FName::from_slice("Class"),
                &FName::from_slice("SCS_Node"),
            )
            .ok_or(io::Error::new(ErrorKind::Other, "No such import"))?;
        let default_scs_node_import = asset
            .find_import_no_index(
                &FName::from_slice("/Script/CoreUObject"),
                &FName::from_slice("SCS_Node"),
                &FName::from_slice("Default__SCS_Node"),
            )
            .ok_or(io::Error::new(ErrorKind::Other, "No such import"))?;
        let none_ref = asset
            .search_name_reference(&String::from("None"))
            .ok_or(io::Error::new(
                ErrorKind::Other,
                "Name reference to \"None\" not found",
            ))?
            .to_le_bytes();
        asset.add_fname("bAutoActivate");

        for component_path_raw in components {
            let mut object_property_template = object_property_template.clone();
            let mut template_export = template_export.clone();
            let mut scs_node_template = scs_node_template.clone();

            let mut component_path = component_path_raw.as_str();
            let component = Path::new(component_path_raw)
                .file_stem()
                .map(|e| e.to_str())
                .flatten()
                .ok_or(io::Error::new(
                    ErrorKind::Other,
                    "Invalid linked actor component",
                ))?;

            let (component_path, component) = match component.contains(".") {
                true => {
                    let split: Vec<&str> = component.split(".").collect();
                    (split[0], &split[1][..split[1].len() - 2])
                }
                false => (component_path, component_path),
            };

            asset.add_fname(component_path);
            asset.add_name_reference(String::from("Default__") + component + "_C", false);
            asset.add_name_reference(String::from(component) + "_C", false);
            asset.add_name_reference(String::from(component) + "_GEN_VARIABLE", false);
            asset.add_fname(component);
            asset.add_fname("SCS_Node");

            let package_import = Import {
                class_package: FName::from_slice("/Script/CoreUObject"),
                class_name: FName::from_slice("Package"),
                outer_index: PackageIndex::new(0),
                object_name: FName::from_slice(component_path),
            };
            let package_import = asset.add_import(package_import);

            let blueprint_generated_class_import = Import {
                class_package: FName::from_slice("/Script/Engine"),
                class_name: FName::from_slice("BlueprintGeneratedClass"),
                outer_index: package_import,
                object_name: FName::new(String::from(component) + "_C", 0),
            };
            let blueprint_generated_class_import =
                asset.add_import(blueprint_generated_class_import);

            let default_import = Import {
                class_package: FName::from_slice(component_path),
                class_name: FName::new(String::from(component) + "_C", 0),
                outer_index: package_import,
                object_name: FName::new(String::from("Default__") + component + "_C", 0),
            };
            let default_import = asset.add_import(default_import);

            template_export.base_export.class_index = blueprint_generated_class_import;
            template_export.base_export.object_name =
                FName::new(String::from(component) + "_GEN_VARIABLE", 0);
            template_export.base_export.template_index = default_import;

            object_property_template.base_export.class_index =
                PackageIndex::new(object_property_import);
            object_property_template.base_export.object_name = FName::from_slice("SCS_Node");
            object_property_template.base_export.template_index =
                PackageIndex::new(default_scs_node_import);

            let mut raw_data = Vec::new();

            // Here we specify the raw data for our ObjectProperty category, including necessary flags and such
            // magic numbers?
            raw_data.extend(none_ref);
            raw_data.extend(vec![
                0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x00,
                0x00, 0x00,
            ]);
            raw_data.extend(none_ref);
            raw_data.push(0);
            raw_data.extend(blueprint_generated_class_import.index.to_le_bytes());

            object_property_template.base_export.outer_index = PackageIndex::new(bgc_location + 1);
            template_export.base_export.outer_index = PackageIndex::new(bgc_location + 1);
            scs_node_template.base_export.outer_index = PackageIndex::new(scs_location + 1);

            template_export
                .base_export
                .serialization_before_serialization_dependencies
                .push(PackageIndex::new(bgc_location + 1));
            template_export
                .base_export
                .serialization_before_create_dependencies
                .push(blueprint_generated_class_import);
            template_export
                .base_export
                .serialization_before_create_dependencies
                .push(default_import);
            template_export
                .base_export
                .create_before_create_dependencies
                .push(PackageIndex::new(bgc_location + 1));
            template_export.extras = [0u8; 4].to_vec();
            template_export.properties = Vec::from([BoolProperty {
                name: FName::from_slice("bAutoActivate"),
                property_guid: None,
                duplication_index: 0,
                value: true,
            }
            .into()]);
            asset.exports.push(template_export.into());

            let exports_len = asset.exports.len() as i32;
            let cdo_export = cast!(
                Export,
                NormalExport,
                &mut asset.exports[cdo_location as usize]
            )
            .expect("Corrupted memory");
            cdo_export
                .base_export
                .serialization_before_serialization_dependencies
                .push(PackageIndex::new(exports_len));

            object_property_template
                .base_export
                .create_before_serialization_dependencies
                .push(blueprint_generated_class_import);
            object_property_template
                .base_export
                .create_before_create_dependencies
                .push(PackageIndex::new(bgc_location + 1));
            object_property_template.data = raw_data;
            asset.exports.push(object_property_template.into());

            node_offset += 1;
            scs_node_template.base_export.object_name =
                FName::new(String::from("SCS_Node"), node_offset);
            scs_node_template.extras = [0u8; 4].to_vec();
            scs_node_template
                .base_export
                .create_before_serialization_dependencies
                .push(blueprint_generated_class_import);
            scs_node_template
                .base_export
                .create_before_serialization_dependencies
                .push(PackageIndex::new(asset.exports.len() as i32 - 1));
            scs_node_template
                .base_export
                .serialization_before_create_dependencies
                .push(PackageIndex::new(scs_node_import));
            scs_node_template
                .base_export
                .serialization_before_create_dependencies
                .push(PackageIndex::new(default_scs_node_import));
            scs_node_template
                .base_export
                .create_before_create_dependencies
                .push(PackageIndex::new(scs_location + 1));
            scs_node_template.properties = Vec::from([
                ObjectProperty {
                    name: FName::from_slice("ComponentClass"),
                    property_guid: None,
                    duplication_index: 0,
                    value: blueprint_generated_class_import,
                }
                .into(),
                ObjectProperty {
                    name: FName::from_slice("ComponentTemplate"),
                    property_guid: None,
                    duplication_index: 0,
                    value: PackageIndex::new(asset.exports.len() as i32 - 1),
                }
                .into(),
                StructProperty {
                    name: FName::from_slice("VariableGuid"),
                    struct_type: Some(FName::from_slice("Guid")),
                    struct_guid: None,
                    property_guid: None,
                    duplication_index: 0,
                    serialize_none: false,
                    value: Vec::from([GuidProperty {
                        name: FName::from_slice("VariableGuid"),
                        property_guid: None,
                        duplication_index: 0,
                        value: Uuid::new_v4().as_bytes().to_owned(),
                    }
                    .into()]),
                }
                .into(),
                NameProperty {
                    name: FName::from_slice("InternalVariableName"),
                    property_guid: None,
                    duplication_index: 0,
                    value: FName::from_slice(component),
                }
                .into(),
            ]);
            asset.exports.push(scs_node_template.into());
            let scs_node_template_index = asset.exports.len() - 1;

            let exports_len = asset.exports.len() as i32;
            let bgc = cast!(
                Export,
                StructExport,
                &mut asset.exports[bgc_location as usize]
            )
            .expect("Corrupted memory");
            bgc.children.push(PackageIndex::new(exports_len - 1));

            let scs_export = cast!(
                Export,
                NormalExport,
                &mut asset.exports[scs_location as usize]
            )
            .expect("Corrupted memory");

            scs_export
                .base_export
                .create_before_serialization_dependencies
                .push(PackageIndex::new(exports_len));
            scs_export
                .base_export
                .serialization_before_serialization_dependencies
                .push(PackageIndex::new(exports_len));

            let mut new_scs_node_name_index = None;
            for property in &mut scs_export.properties {
                if let Some(array_property) = cast!(Property, ArrayProperty, property) {
                    match array_property.name.content.as_str() {
                        "AllNodes" | "RootNodes" => {
                            new_scs_node_name_index = Some(array_property.value.len() as i32 + 1);
                            array_property.value.push(
                                ObjectProperty {
                                    name: array_property.name.clone(),
                                    property_guid: None,
                                    duplication_index: 0,
                                    value: PackageIndex::new(exports_len), // SCS_Node
                                }
                                .into(),
                            )
                        }
                        _ => {}
                    }
                }
            }
            let new_scs_node_name_index = new_scs_node_name_index
                .ok_or(io::Error::new(ErrorKind::Other, "Corrupted ActorTemplate"))?;
            cast!(
                Export,
                NormalExport,
                &mut asset.exports[scs_node_template_index]
            )
            .expect("Corrupted memory")
            .base_export
            .object_name
            .index = new_scs_node_name_index;
        }

        write_asset(integrated_pak, &asset, &name)
            .map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;
    }
    Ok(())
}

fn handle_item_list_entries(
    _data: &(),
    integrated_pak: &mut PakFile,
    game_paks: &mut Vec<PakFile>,
    item_list_entires_maps: Vec<&serde_json::Value>,
) -> Result<(), io::Error> {
    let mut new_items = HashMap::new();

    for item_list_entries_map in &item_list_entires_maps {
        let item_list_entries_map = item_list_entries_map.as_object().ok_or(io::Error::new(
            ErrorKind::Other,
            "Invalid item_list_entries",
        ))?;

        for (name, item_list_entries) in item_list_entries_map {
            let item_list_entries = item_list_entries.as_object().ok_or(io::Error::new(
                ErrorKind::Other,
                "Invalid item_list_entries",
            ))?;
            let new_items_entry = new_items
                .entry(name.clone())
                .or_insert_with(|| HashMap::new());

            for (item_name, entries) in item_list_entries {
                let entries = entries.as_array().ok_or(io::Error::new(
                    ErrorKind::Other,
                    "Invalid item_list_entries",
                ))?;

                let new_items_entry_map = new_items_entry
                    .entry(item_name.clone())
                    .or_insert_with(|| Vec::new());
                for entry in entries {
                    let entry = entry.as_str().ok_or(io::Error::new(
                        ErrorKind::Other,
                        "Invalid item_list_entries",
                    ))?;
                    new_items_entry_map.push(String::from(entry));
                }
            }
        }
    }

    for (name, entries) in &new_items {
        let name = game_to_absolute(&name)
            .ok_or(io::Error::new(ErrorKind::Other, "Invalid asset name"))?;
        let mut asset = get_asset(integrated_pak, game_paks, &name, VER_UE4_23)?;
        let mut item_types_property = HashMap::new();

        for i in 0..asset.exports.len() {
            let export = &asset.exports[i];
            if let Some(normal_export) = cast!(Export, NormalExport, export) {
                for property in &normal_export.properties {
                    for (name, _) in &new_items {
                        let arr_name = match name.contains(".") {
                            true => {
                                let split: Vec<&str> = name.split(".").collect();
                                let category_name = split[0];
                                let object_name =
                                    match normal_export.base_export.class_index.is_import() {
                                        true => asset
                                            .get_import(normal_export.base_export.class_index)
                                            .map(|e| e.object_name.content.clone())
                                            .ok_or(io::Error::new(
                                                ErrorKind::Other,
                                                "No such import",
                                            ))?,
                                        false => String::new(),
                                    };
                                if object_name != category_name {
                                    continue;
                                }
                                String::from(split[1])
                            }
                            false => name.clone(),
                        };

                        if let Some(array_property) = cast!(Property, ArrayProperty, property) {
                            if array_property.name.content == arr_name {
                                item_types_property
                                    .entry(name.clone())
                                    .or_insert_with(|| Vec::new())
                                    .push((
                                        i,
                                        array_property
                                            .array_type
                                            .as_ref()
                                            .map(|e| e.content.clone())
                                            .ok_or(io::Error::new(
                                                ErrorKind::Other,
                                                "Unknown array type",
                                            ))?,
                                        name.clone(),
                                    ));
                            }
                        }
                    }
                }
            }
        }

        for (name, paths) in entries {
            if !item_types_property.contains_key(name) {
                continue;
            }

            for item_path in paths {
                let (real_name, class_name, soft_class_name) = match item_path.contains(".") {
                    true => {
                        let split: Vec<&str> = item_path.split(".").collect();
                        (
                            String::from(split[0]),
                            String::from(split[1]),
                            String::from(split[1]),
                        )
                    }
                    false => (
                        item_path.clone(),
                        Path::new(item_path)
                            .file_stem()
                            .map(|e| e.to_str())
                            .flatten()
                            .map(|e| String::from(e) + "_C")
                            .ok_or(io::Error::new(ErrorKind::Other, "Invalid item_path"))?,
                        Path::new(item_path)
                            .file_stem()
                            .map(|e| e.to_str())
                            .flatten()
                            .map(|e| String::from(e))
                            .ok_or(io::Error::new(ErrorKind::Other, "Invalid item_path"))?,
                    ),
                };

                let mut blueprint_generated_class_import = None;

                for (export_index, array_type, name) in
                    item_types_property.get(item_path).unwrap()
                {
                    match array_type.as_str() {
                        "ObjectProperty" => {
                            if blueprint_generated_class_import.is_none() {
                                asset.add_fname(&real_name);
                                asset.add_fname(&class_name);

                                let package_import = Import {
                                    class_package: FName::from_slice("/Script/CoreUObject"),
                                    class_name: FName::from_slice("Package"),
                                    outer_index: PackageIndex::new(0),
                                    object_name: FName::from_slice(&real_name),
                                };
                                let package_import = asset.add_import(package_import);

                                let new_import = Import {
                                    class_package: FName::from_slice("/Script/Engine"),
                                    class_name: FName::from_slice("BlueprintGeneratedClass"),
                                    outer_index: package_import,
                                    object_name: FName::from_slice(&class_name),
                                };
                                blueprint_generated_class_import =
                                    Some(asset.add_import(new_import));
                            }

                            let export = asset
                                .exports
                                .get_mut(*export_index)
                                .map(|e| cast!(Export, NormalExport, e))
                                .flatten()
                                .expect("Corrupted memory");
                            export.properties.push(
                                ObjectProperty {
                                    name: FName::from_slice(name),
                                    property_guid: None,
                                    duplication_index: 0,
                                    value: blueprint_generated_class_import.unwrap(),
                                }
                                .into(),
                            );
                        }
                        "SoftObjectProperty" => {
                            asset.add_fname(&real_name);
                            asset.add_name_reference(real_name.clone() + "." + &soft_class_name, false);

                            let export = asset
                                .exports
                                .get_mut(*export_index)
                                .map(|e| cast!(Export, NormalExport, e))
                                .flatten()
                                .expect("Corrupted memory");
                            
                            export.properties.push(SoftObjectProperty {
                                name: FName::from_slice(name),
                                property_guid: None,
                                duplication_index: 0,
                                value: FName::new(real_name.clone() + "." + &soft_class_name, 0),
                                id: 0,
                            }.into());
                        }
                        _ => {}
                    }
                }
            }
        }
        write_asset(integrated_pak, &asset, &name)
            .map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;
    }
    Ok(())
}
impl<'data> IntegratorConfig<'data, (), io::Error> for AstroIntegratorConfig {
    fn get_data(&self) -> &'data () {
        &()
    }

    fn get_handlers(
        &self,
    ) -> std::collections::HashMap<
        String,
        Box<
            dyn FnMut(
                &(),
                &mut unreal_pak::PakFile,
                &mut Vec<unreal_pak::PakFile>,
                Vec<&serde_json::Value>,
            ) -> Result<(), io::Error>,
        >,
    > {
        let mut handlers: std::collections::HashMap<
            String,
            Box<
                dyn FnMut(
                    &(),
                    &mut unreal_pak::PakFile,
                    &mut Vec<unreal_pak::PakFile>,
                    Vec<&serde_json::Value>,
                ) -> Result<(), io::Error>,
            >,
        > = HashMap::new();

        handlers.insert(
            String::from("persistent_actors"),
            Box::new(handle_persistent_actors),
        );

        handlers.insert(
            String::from("mission_trailheads"),
            Box::new(handle_mission_trailheads),
        );

        handlers.insert(
            String::from("linked_actor_components"),
            Box::new(handle_linked_actor_components),
        );

        handlers.insert(
            String::from("item_list_entries"),
            Box::new(handle_item_list_entries),
        );

        handlers
    }

    fn get_game_name(&self) -> String {
        "Astro".to_string()
    }

    fn get_integrator_version(&self) -> String {
        String::from("0.1.0")
    }

    fn get_engine_version(&self) -> i32 {
        VER_UE4_23
    }
}
