//! P2.5 — CLIP attribute facets.
//!
//! A curated catalog of zero-shot object *attributes* — vehicle colour, vehicle
//! type, and person/clothing colour — each mapped to a stable `key`, a display
//! `label`, and a CLIP text `prompt`. It is reused two ways over the
//! ALREADY-STORED crop embeddings (Re-ID), so it costs **zero new inference**:
//!
//! * the Events "Attributes" filter (`GET /api/search/by-attr`) CLIP-text-embeds
//!   the chosen facet's prompt once and cosine-ranks the crop corpus, and
//! * the `attr_like` alarm condition — a near-verbatim generalisation of the
//!   P2.2 prompt rule: the rule stores a catalog *key* (not free text), which
//!   [`crate::db::AlarmRule::effective_prompt`] resolves to this prompt so the
//!   pipeline's embedding pass compares it against each detection's crop exactly
//!   like `prompt_like`.
//!
//! Make/model recognition is deliberately OUT of scope (needs a dedicated model,
//! not zero-shot CLIP). Both surfaces are best-effort semantic matches — the
//! same honest "AI watch"-style framing as `prompt_like`.

/// One facet: a stable catalog key, a short display label (shown under its
/// group), and the CLIP text prompt embedded to match it.
pub struct Facet {
    pub key: &'static str,
    pub label: &'static str,
    pub prompt: &'static str,
}

/// A group of related facets (e.g. "Vehicle colour"), for grouped UI rendering.
pub struct FacetGroup {
    /// Stable group id (kebab-case).
    pub group: &'static str,
    /// Human-readable group heading.
    pub label: &'static str,
    pub attrs: Vec<Facet>,
}

fn f(key: &'static str, label: &'static str, prompt: &'static str) -> Facet {
    Facet { key, label, prompt }
}

/// The curated facet catalog (lazily built once; the borrow is `'static`).
pub fn catalog() -> &'static [FacetGroup] {
    use std::sync::OnceLock;
    static CATALOG: OnceLock<Vec<FacetGroup>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            vec![
                FacetGroup {
                    group: "vehicle-color",
                    label: "Vehicle colour",
                    attrs: vec![
                        f("veh_color_red", "Red", "a red car"),
                        f("veh_color_white", "White", "a white car"),
                        f("veh_color_black", "Black", "a black car"),
                        f("veh_color_blue", "Blue", "a blue car"),
                        f("veh_color_silver", "Silver", "a silver car"),
                        f("veh_color_gray", "Gray", "a gray car"),
                        f("veh_color_green", "Green", "a green car"),
                        f("veh_color_yellow", "Yellow", "a yellow car"),
                        f("veh_color_brown", "Brown", "a brown car"),
                        f("veh_color_orange", "Orange", "an orange car"),
                    ],
                },
                FacetGroup {
                    group: "vehicle-type",
                    label: "Vehicle type",
                    attrs: vec![
                        f("veh_type_sedan", "Sedan", "a sedan car"),
                        f("veh_type_suv", "SUV", "an SUV"),
                        f("veh_type_pickup", "Pickup truck", "a pickup truck"),
                        f("veh_type_van", "Van", "a van"),
                        f("veh_type_hatchback", "Hatchback", "a hatchback car"),
                        f("veh_type_bus", "Bus", "a bus"),
                        f("veh_type_motorcycle", "Motorcycle", "a motorcycle"),
                        f("veh_type_bicycle", "Bicycle", "a bicycle"),
                    ],
                },
                FacetGroup {
                    group: "person-color",
                    label: "Person / clothing colour",
                    attrs: vec![
                        f(
                            "person_color_red",
                            "In red",
                            "a person wearing red clothing",
                        ),
                        f(
                            "person_color_blue",
                            "In blue",
                            "a person wearing blue clothing",
                        ),
                        f(
                            "person_color_black",
                            "In black",
                            "a person wearing black clothing",
                        ),
                        f(
                            "person_color_white",
                            "In white",
                            "a person wearing white clothing",
                        ),
                        f(
                            "person_color_green",
                            "In green",
                            "a person wearing green clothing",
                        ),
                        f(
                            "person_color_yellow",
                            "In yellow",
                            "a person wearing yellow clothing",
                        ),
                        f(
                            "person_color_gray",
                            "In gray",
                            "a person wearing gray clothing",
                        ),
                        f(
                            "person_color_orange",
                            "In orange",
                            "a person wearing orange clothing",
                        ),
                    ],
                },
            ]
        })
        .as_slice()
}

/// Resolve a facet key to its CLIP text prompt. `None` for an unknown key — e.g.
/// a stored `attr_like` whose catalog entry was later removed.
pub fn prompt_for(key: &str) -> Option<&'static str> {
    let key = key.trim();
    catalog()
        .iter()
        .flat_map(|g| g.attrs.iter())
        .find(|a| a.key == key)
        .map(|a| a.prompt)
}

/// Whether `key` names a facet in the catalog (used by rule validation).
pub fn is_known(key: &str) -> bool {
    prompt_for(key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_keys_are_unique_and_resolvable() {
        let mut seen = std::collections::HashSet::new();
        let mut count = 0;
        for g in catalog() {
            assert!(!g.attrs.is_empty(), "group {} has no facets", g.group);
            for a in &g.attrs {
                assert!(seen.insert(a.key), "duplicate facet key {}", a.key);
                assert!(!a.prompt.trim().is_empty(), "empty prompt for {}", a.key);
                // Every catalog key resolves back to its own prompt.
                assert_eq!(prompt_for(a.key), Some(a.prompt));
                count += 1;
            }
        }
        // A meaningful catalog; guards against an accidental truncation.
        assert!(count >= 20, "expected a substantial catalog, got {count}");
        assert!(is_known("veh_color_red"));
        assert!(!is_known("veh_color_teal"));
        assert_eq!(prompt_for("  veh_type_suv  "), Some("an SUV"));
        assert_eq!(prompt_for("nope"), None);
    }
}
