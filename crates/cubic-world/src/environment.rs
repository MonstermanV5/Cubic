use cubic_version::{GameData, MinecraftVersionId};

use crate::RuntimeBlockStateId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FluidKind {
    Water,
    Lava,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidState {
    pub kind: FluidKind,
    pub level: u8,
    pub falling: bool,
}

impl FluidState {
    #[must_use]
    pub fn own_height(self) -> f64 {
        // FlowingFluid exposes amount / 9 to rendering. A falling state still
        // carries amount eight; it is not a geometrically full-height source.
        f64::from(8_u8.saturating_sub(self.level.min(7))) / 9.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SpecialSurface {
    #[default]
    Ordinary,
    Ice,
    PackedIce,
    BlueIce,
    FrostedIce,
    Slime,
    Honey,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BlockEnvironment {
    pub fluid: Option<FluidState>,
    pub climbable: bool,
    pub scaffolding: bool,
    pub surface: SpecialSurface,
    pub emissive: bool,
}

#[derive(Clone, Debug, Default)]
pub struct BlockEnvironmentProfile {
    states: Vec<BlockEnvironment>,
}

impl BlockEnvironmentProfile {
    #[must_use]
    pub fn from_game_data(data: &GameData) -> Self {
        let rules = EnvironmentRuleSet::for_version(&data.artifact().minecraft_version);
        let capacity = data
            .artifact()
            .blocks
            .iter()
            .flat_map(|block| block.states.iter().map(|state| state.state_id))
            .max()
            .and_then(|maximum| usize::try_from(maximum).ok())
            .and_then(|maximum| maximum.checked_add(1))
            .unwrap_or(0);
        let mut states = vec![BlockEnvironment::default(); capacity];
        for block in &data.artifact().blocks {
            let path = block
                .identifier
                .as_str()
                .split_once(':')
                .map_or(block.identifier.as_str(), |(_, path)| path);
            for state in &block.states {
                if let Ok(index) = usize::try_from(state.state_id)
                    && let Some(slot) = states.get_mut(index)
                {
                    *slot = rules.resolve(path, &state.properties);
                }
            }
        }
        Self { states }
    }

    #[must_use]
    pub fn state(&self, state: RuntimeBlockStateId) -> BlockEnvironment {
        usize::try_from(state.0)
            .ok()
            .and_then(|index| self.states.get(index))
            .copied()
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn synthetic(
        states: impl IntoIterator<Item = (RuntimeBlockStateId, BlockEnvironment)>,
    ) -> Self {
        let entries = states.into_iter().collect::<Vec<_>>();
        let capacity = entries
            .iter()
            .filter_map(|(state, _)| usize::try_from(state.0).ok())
            .max()
            .and_then(|maximum| maximum.checked_add(1))
            .unwrap_or(0);
        let mut result = vec![BlockEnvironment::default(); capacity];
        for (state, environment) in entries {
            if let Ok(index) = usize::try_from(state.0)
                && let Some(slot) = result.get_mut(index)
            {
                *slot = environment;
            }
        }
        Self { states: result }
    }
}

#[derive(Clone, Copy)]
enum EnvironmentRuleSet {
    Java26_1_2,
    Conservative,
}

impl EnvironmentRuleSet {
    fn for_version(version: &MinecraftVersionId) -> Self {
        if version.as_str() == "26.1.2" {
            Self::Java26_1_2
        } else {
            Self::Conservative
        }
    }

    fn resolve(
        self,
        path: &str,
        properties: &std::collections::BTreeMap<String, String>,
    ) -> BlockEnvironment {
        if matches!(self, Self::Conservative) {
            return BlockEnvironment::default();
        }
        let waterlogged = properties
            .get("waterlogged")
            .is_some_and(|value| value == "true");
        // These 26.1.2 blocks occupy a water fluid cell even though their
        // block states do not expose the generic `waterlogged` property. The
        // exact-version adapter resolves that resource fact once; generic
        // rendering and movement consume only `FluidState`.
        let intrinsically_water_filled = matches!(
            path,
            "seagrass" | "tall_seagrass" | "kelp" | "kelp_plant" | "bubble_column"
        );
        let fluid = if path == "water" || waterlogged || intrinsically_water_filled {
            Some(fluid_state(FluidKind::Water, properties))
        } else if path == "lava" {
            Some(fluid_state(FluidKind::Lava, properties))
        } else {
            None
        };
        let climbable = matches!(
            path,
            "ladder"
                | "vine"
                | "weeping_vines"
                | "weeping_vines_plant"
                | "twisting_vines"
                | "twisting_vines_plant"
                | "cave_vines"
                | "cave_vines_plant"
        );
        let surface = match path {
            "ice" => SpecialSurface::Ice,
            "packed_ice" => SpecialSurface::PackedIce,
            "blue_ice" => SpecialSurface::BlueIce,
            "frosted_ice" => SpecialSurface::FrostedIce,
            "slime_block" => SpecialSurface::Slime,
            "honey_block" => SpecialSurface::Honey,
            _ => SpecialSurface::Ordinary,
        };
        BlockEnvironment {
            fluid,
            climbable,
            scaffolding: path == "scaffolding",
            surface,
            emissive: matches!(path, "lava" | "fire" | "soul_fire" | "glowstone"),
        }
    }
}

fn fluid_state(
    kind: FluidKind,
    properties: &std::collections::BTreeMap<String, String>,
) -> FluidState {
    let raw = properties
        .get("level")
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(0);
    FluidState {
        kind,
        // LiquidBlock's 26.1.2 cache maps encoded falling levels 8..=15 to
        // flowing amount eight. Cubic stores the inverse amount as `level`,
        // so falling columns carry level zero rather than level seven.
        level: if raw >= 8 { 0 } else { raw },
        falling: raw >= 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fluid_height_and_unknown_version_rules_are_explicit() {
        let profile = BlockEnvironmentProfile::synthetic([(
            RuntimeBlockStateId(7),
            BlockEnvironment {
                surface: SpecialSurface::Honey,
                ..BlockEnvironment::default()
            },
        )]);
        assert_eq!(
            profile.state(RuntimeBlockStateId(7)).surface,
            SpecialSurface::Honey
        );
        assert_eq!(
            FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: false,
            }
            .own_height(),
            8.0 / 9.0
        );
        let falling = fluid_state(
            FluidKind::Water,
            &std::collections::BTreeMap::from([("level".to_owned(), "15".to_owned())]),
        );
        assert!(falling.falling);
        assert_eq!(falling.level, 0);
        assert_eq!(falling.own_height(), 8.0 / 9.0);
        for raw in 0_u8..=15 {
            let decoded = fluid_state(
                FluidKind::Water,
                &std::collections::BTreeMap::from([("level".to_owned(), raw.to_string())]),
            );
            let expected_level = if raw >= 8 { 0 } else { raw };
            assert_eq!(decoded.level, expected_level, "raw={raw}");
            assert_eq!(decoded.falling, raw >= 8, "raw={raw}");
            assert_eq!(
                decoded.own_height(),
                f64::from(8 - expected_level) / 9.0,
                "raw={raw}"
            );
        }
        assert_eq!(
            FluidState {
                kind: FluidKind::Lava,
                level: 7,
                falling: false,
            }
            .own_height(),
            1.0 / 9.0
        );
        assert_eq!(
            FluidState {
                kind: FluidKind::Water,
                level: 0,
                falling: true,
            }
            .own_height(),
            8.0 / 9.0
        );
        assert_eq!(
            EnvironmentRuleSet::Conservative.resolve("water", &Default::default()),
            BlockEnvironment::default()
        );
    }

    #[test]
    fn exact_adapter_resolves_intrinsic_and_property_water_cells() {
        let rules = EnvironmentRuleSet::Java26_1_2;
        for path in [
            "seagrass",
            "tall_seagrass",
            "kelp",
            "kelp_plant",
            "bubble_column",
        ] {
            assert_eq!(
                rules.resolve(path, &Default::default()).fluid,
                Some(FluidState {
                    kind: FluidKind::Water,
                    level: 0,
                    falling: false,
                }),
                "{path}"
            );
        }
        let waterlogged =
            std::collections::BTreeMap::from([("waterlogged".to_owned(), "true".to_owned())]);
        assert_eq!(
            rules
                .resolve("oak_slab", &waterlogged)
                .fluid
                .map(|fluid| fluid.kind),
            Some(FluidKind::Water)
        );
    }
}
