//! Making room in a full process-wide map without clearing it: a flood in one server must never
//! wipe the detection state (heat, pings, live captchas, cooldowns) of the others.
use std::collections::HashMap;
use std::hash::Hash;

/// Drops every entry of the group (a server, a rule) that holds the most entries.
pub fn drop_biggest<K: Eq + Hash, V, G: Eq + Hash + Copy>(
    map: &mut HashMap<K, V>,
    group: impl Fn(&K) -> G,
) {
    let mut sizes: HashMap<G, usize> = HashMap::new();
    for k in map.keys() {
        *sizes.entry(group(k)).or_default() += 1;
    }
    if let Some((top, _)) = sizes.into_iter().max_by_key(|(_, n)| *n) {
        map.retain(|k, _| group(k) != top);
    }
}

/// The median of snowflakes (message ids grow with time): keeping ids at or above it drops the
/// older half, whichever servers they came from.
pub fn newer_half(ids: impl Iterator<Item = u64>) -> u64 {
    let mut v: Vec<u64> = ids.collect();
    if v.is_empty() {
        return 0;
    }
    let mid = v.len() / 2;
    *v.select_nth_unstable(mid).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flood_in_one_server_leaves_the_others() {
        let mut m: HashMap<(u64, u64), ()> = (0..10).map(|u| ((1, u), ())).collect();
        m.insert((2, 1), ());
        m.insert((3, 1), ());
        drop_biggest(&mut m, |k| k.0);
        let mut left: Vec<_> = m.keys().copied().collect();
        left.sort();
        assert_eq!(left, vec![(2, 1), (3, 1)]);
    }

    #[test]
    fn the_older_half_goes() {
        let cut = newer_half([5, 1, 9, 3, 7].into_iter());
        assert_eq!(cut, 5);
        assert_eq!(newer_half(std::iter::empty()), 0);
    }
}
