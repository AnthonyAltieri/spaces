use rand::seq::SliceRandom;
use rand::Rng;
use std::collections::HashSet;

const ADJECTIVES: &[&str] = &[
    "amber", "brisk", "calm", "clever", "dapper", "eager", "ember", "fable", "granite", "harbor",
    "jolly", "kind", "lively", "maple", "merry", "nimble", "orchid", "pebble", "plucky", "rapid",
    "river", "spruce", "steady", "sunny", "tidy", "vivid",
];

const NOUNS: &[&str] = &[
    "anchor", "badger", "breeze", "cedar", "comet", "falcon", "forest", "harbor", "meadow",
    "otter", "panda", "pine", "quartz", "ridge", "rocket", "signal", "sparrow", "summit",
    "thicket", "trail", "voyage", "willow", "wren",
];

pub fn generate_workspace_name(existing: &HashSet<String>) -> String {
    let mut rng = rand::thread_rng();
    generate_workspace_name_with_rng(existing, &mut rng)
}

pub(crate) fn generate_workspace_name_with_rng<R>(existing: &HashSet<String>, rng: &mut R) -> String
where
    R: Rng + ?Sized,
{
    for _ in 0..64 {
        let candidate = format!(
            "{}-{}",
            ADJECTIVES.choose(rng).expect("adjective list is non-empty"),
            NOUNS.choose(rng).expect("noun list is non-empty")
        );
        if !existing.contains(&candidate) {
            return candidate;
        }
    }

    let base = format!(
        "{}-{}",
        ADJECTIVES.choose(rng).expect("adjective list is non-empty"),
        NOUNS.choose(rng).expect("noun list is non-empty")
    );

    let mut suffix = 2_u32;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{generate_workspace_name_with_rng, ADJECTIVES, NOUNS};
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::collections::HashSet;

    #[test]
    fn generates_memorable_slug() {
        let mut rng = StdRng::seed_from_u64(42);
        let name = generate_workspace_name_with_rng(&HashSet::new(), &mut rng);
        let parts: Vec<_> = name.split('-').collect();

        assert_eq!(parts.len(), 2);
        assert!(ADJECTIVES.contains(&parts[0]));
        assert!(NOUNS.contains(&parts[1]));
    }

    #[test]
    fn adds_numeric_suffix_after_collision() {
        let mut rng = StdRng::seed_from_u64(7);
        let base = generate_workspace_name_with_rng(&HashSet::new(), &mut rng);

        let mut existing = HashSet::new();
        existing.insert(base.clone());

        let mut rerolled_rng = StdRng::seed_from_u64(7);
        let second = generate_workspace_name_with_rng(&existing, &mut rerolled_rng);

        assert_ne!(base, second);
        assert!(second.starts_with(&format!("{base}-")) || second != base);
    }
}
