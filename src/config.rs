use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::path::Path;
use crate::plugin::PluginSetting;

/// Parse a single string into a plugin setting.
/// Used by CLI and YAML parsing.
pub fn parse_plugin_setting(s: &str) -> Result<PluginSetting, String> {
    let parts: Vec<&str> = s.split(':').collect();
    let mut plugin_identifier = String::new();
    
    let mut i = 0;
    while i < parts.len() {
        if parts[i].contains('=') {
            break;
        }
        if !plugin_identifier.is_empty() {
            plugin_identifier.push(':');
        }
        plugin_identifier.push_str(parts[i].trim());
        i += 1;
    }

    if plugin_identifier.is_empty() {
        return Err("Missing plugin identifier".to_string());
    }

    let mut params = HashMap::new();
    for part in parts.iter().skip(i) {
        if part.trim().is_empty() {
            continue;
        }
        match parse_key_value(part) {
            Ok((k, v)) => {
                params.insert(k, v);
            }
            Err(e) => {
                return Err(format!("Malformed parameter '{}': {}", part, e));
            }
        }
    }
    Ok(PluginSetting {
        plugin_identifier,
        params,
    })
}

pub fn parse_key_value(s: &str) -> Result<(String, f32)> {
    let kv: Vec<&str> = s.split('=').collect();
    if kv.len() == 2 {
        let key = kv[0].trim().to_string();
        let value = kv[1].parse::<f32>().map_err(|_| anyhow!("Invalid numeric value: {}", kv[1]))?;
        if !value.is_finite() {
            bail!("Parameter '{}' must be a finite number", key);
        }
        Ok((key, value))
    } else {
        bail!("Expected key=value pair")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_value_validation() {
        assert!(parse_key_value("gain=1.0").is_ok());
        assert!(parse_key_value("gain=NaN").is_err());
        assert!(parse_key_value("gain=inf").is_err());
        assert!(parse_key_value("gain=-inf").is_err());
    }
}

pub fn load_chain_from_yaml(config_path: &Path) -> Result<Vec<PluginSetting>> {
    let file = std::fs::File::open(config_path)
        .map_err(|e| anyhow!("Failed to open config file {:?}: {}", config_path, e))?;
    
    let yaml_val: serde_yaml::Value = serde_yaml::from_reader(file)
        .map_err(|e| anyhow!("Failed to parse YAML {:?}: {}", config_path, e))?;
    
    let mut chain = Vec::new();
    let items = if let Some(p) = yaml_val.get("plugins") {
        p.as_sequence().ok_or_else(|| anyhow!("'plugins' must be a list"))?
    } else if let Some(s) = yaml_val.as_sequence() {
        s
    } else {
        bail!("YAML must be a list of plugins or contain a 'plugins' key");
    };

    for item in items {
        if let Some(s) = item.as_str() {
            chain.push(parse_plugin_setting(s).map_err(|e| anyhow!(e))?);
        } else if let Some(m) = item.as_mapping() {
            for (k, v) in m {
                let name = k.as_str().ok_or_else(|| anyhow!("Plugin name must be a string"))?;
                let mut params = HashMap::new();
                
                if let Some(param_str) = v.as_str() {
                    add_params_from_str(&mut params, param_str)?;
                } else if let Some(param_list) = v.as_sequence() {
                    for p in param_list {
                        if let Some(ps) = p.as_str() {
                            add_params_from_str(&mut params, ps)?;
                        }
                    }
                } else if let Some(param_map) = v.as_mapping() {
                    for (pk, pv) in param_map {
                        let p_name = pk.as_str().ok_or_else(|| anyhow!("Param name must be a string"))?;
                        let p_val = pv.as_f64().ok_or_else(|| anyhow!("Param value for '{}' must be a number", p_name))? as f32;
                        if !p_val.is_finite() {
                            bail!("Parameter '{}' must be a finite number", p_name);
                        }
                        params.insert(p_name.to_string(), p_val);
                    }
                }
                
                chain.push(PluginSetting {
                    plugin_identifier: name.to_string(),
                    params,
                });
            }
        }
    }
    Ok(chain)
}

fn add_params_from_str(params: &mut HashMap<String, f32>, s: &str) -> Result<()> {
    if s.contains(':') {
        for part in s.split(':') {
            if part.trim().is_empty() {
                continue;
            }
            let (k, v) = parse_key_value(part)?;
            params.insert(k, v);
        }
    } else {
        let (k, v) = parse_key_value(s)?;
        params.insert(k, v);
    }
    Ok(())
}
