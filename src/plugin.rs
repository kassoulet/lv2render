use anyhow::{bail, Result};
use livi::{Instance, Plugin, PortType, World, event::LV2AtomSequence};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PluginSetting {
    pub plugin_identifier: String,
    pub params: HashMap<String, f32>,
}

pub struct PluginInstance {
    pub plugin: Plugin,
    pub instance: Instance,
    pub control_inputs: Vec<f32>,
    pub control_outputs: Vec<f32>,
    pub atom_sequence_inputs: Vec<LV2AtomSequence>,
    pub atom_sequence_outputs: Vec<LV2AtomSequence>,
    pub audio_outputs: Vec<Vec<f32>>,
}

pub enum PluginLookup {
    Found(Plugin),
    NotFound,
    Ambiguous(Vec<String>),
}

pub fn find_plugin(world: &World, identifier: &str) -> PluginLookup {
    if let Some(plugin) = world.plugin_by_uri(identifier) {
        return PluginLookup::Found(plugin);
    }
    let matches: Vec<_> = world.iter_plugins()
        .filter(|p| p.name().to_lowercase().contains(&identifier.to_lowercase()))
        .collect();
    
    match matches.len() {
        0 => PluginLookup::NotFound,
        1 => PluginLookup::Found(matches[0].clone()),
        _ => PluginLookup::Ambiguous(matches.into_iter().map(|p| p.name()).collect()),
    }
}

pub fn apply_parameter_settings(plugin: &Plugin, control_inputs: &mut [f32], settings: &HashMap<String, f32>) -> Result<()> {
    let ports: Vec<_> = plugin.ports_with_type(PortType::ControlInput).collect();
    for (param, value) in settings {
        let port_idx = ports.iter().position(|p| p.name.to_lowercase() == param.to_lowercase());
        if let Some(idx) = port_idx {
            control_inputs[idx] = *value;
            println!("  Set parameter '{}' to {}", param, value);
        } else {
            bail!("Parameter '{}' not found in plugin '{}'", param, plugin.name());
        }
    }
    Ok(())
}
