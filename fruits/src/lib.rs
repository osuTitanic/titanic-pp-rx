mod catch_object;
mod curve;
mod difficulty_object;
mod math_util;
mod movement;

use catch_object::CatchObject;
use curve::Curve;
use difficulty_object::DifficultyObject;
use movement::Movement;

use parse::{Beatmap, HitObjectKind, Mods, PathType};
use std::cmp::Ordering;
use std::convert::identity;

const SECTION_LENGTH: f32 = 750.0;
const STAR_SCALING_FACTOR: f32 = 0.153;

const ALLOWED_CATCH_RANGE: f32 = 0.8;
const CATCHER_SIZE: f32 = 106.75;

macro_rules! binary_search {
    ($slice:expr, $target:expr) => {
        $slice.binary_search_by(|p| p.time.partial_cmp(&$target).unwrap_or(Ordering::Equal))
    };
}

/// Star calculation for osu!ctb maps
// Slider parsing based on https://github.com/osufx/catch-the-pp
pub fn stars(map: &Beatmap, mods: impl Mods) -> DifficultyAttributes {
    if map.hit_objects.len() < 2 {
        return DifficultyAttributes::default();
    }

    let attributes = map.attributes().mods(mods);
    let with_hr = mods.hr();
    let mut ticks = Vec::new(); // using the same buffer for all sliders

    let mut fruits = 0;
    let mut droplets = 0;

    // BUG: Incorrect object order on 2B maps that have fruits within sliders
    let mut hit_objects = map
        .hit_objects
        .iter()
        .scan((None, 0.0), |(last_pos, last_time), h| match &h.kind {
            HitObjectKind::Circle => {
                let mut h = CatchObject::new((h.pos, h.start_time));

                if with_hr {
                    h = h.with_hr(last_pos, last_time);
                }

                fruits += 1;

                Some(Some(FruitOrJuice::Fruit(Some(h))))
            }
            HitObjectKind::Slider {
                pixel_len,
                repeats,
                curve_points,
                path_type,
            } => {
                // HR business
                last_pos
                    .replace(h.pos.x + curve_points[curve_points.len() - 1].x - curve_points[0].x);
                *last_time = h.start_time;

                let (beat_len, timing_time) = {
                    match binary_search!(map.timing_points, h.start_time) {
                        Ok(idx) => {
                            let point = &map.timing_points[idx];
                            (point.beat_len, point.time)
                        }
                        Err(0) => (1000.0, 0.0),
                        Err(idx) => {
                            let point = &map.timing_points[idx - 1];
                            (point.beat_len, point.time)
                        }
                    }
                };

                let (speed_multiplier, diff_time) = {
                    match binary_search!(map.difficulty_points, h.start_time) {
                        Ok(idx) => {
                            let point = &map.difficulty_points[idx];
                            (point.speed_multiplier, point.time)
                        }
                        Err(0) => (1.0, 0.0),
                        Err(idx) => {
                            let point = &map.difficulty_points[idx - 1];
                            (point.speed_multiplier, point.time)
                        }
                    }
                };

                let mut tick_distance = 100.0 * map.sv / map.tick_rate;

                if map.version >= 8 {
                    tick_distance /= (100.0 / speed_multiplier).max(10.0).min(1000.0) / 100.0;
                }

                let spm = if timing_time > diff_time {
                    1.0
                } else {
                    speed_multiplier
                };

                let duration = *repeats as f32 * beat_len * *pixel_len / (map.sv * spm) / 100.0;

                let path_type = if *path_type == PathType::PerfectCurve && curve_points.len() > 3 {
                    PathType::Bezier
                } else if curve_points.len() == 2 {
                    PathType::Linear
                } else {
                    *path_type
                };

                let curve = match path_type {
                    PathType::Linear => Curve::linear(curve_points[0], curve_points[1]),
                    PathType::Bezier => Curve::bezier(curve_points),
                    PathType::Catmull => Curve::catmull(curve_points),
                    PathType::PerfectCurve => Curve::perfect(curve_points),
                };

                let mut current_distance = tick_distance;
                let time_add = duration * (tick_distance / (*pixel_len * *repeats as f32));

                let target = *pixel_len - tick_distance / 8.0;
                ticks.reserve((target / tick_distance) as usize);

                while current_distance < target {
                    let pos = curve.point_at_distance(current_distance);

                    ticks.push((pos, h.start_time + time_add * (ticks.len() + 1) as f32));
                    current_distance += tick_distance;
                }

                let mut slider_objects = Vec::with_capacity(repeats * (ticks.len() + 1));
                slider_objects.push((h.pos, h.start_time));

                if *repeats <= 1 {
                    slider_objects.append(&mut ticks); // automatically empties buffer for next slider
                } else {
                    slider_objects.append(&mut ticks.clone());

                    for repeat_id in 1..*repeats - 1 {
                        let dist = (repeat_id % 2) as f32 * *pixel_len;
                        let time_offset = (duration / *repeats as f32) * repeat_id as f32;
                        let pos = curve.point_at_distance(dist);

                        // Reverse tick / last legacy tick
                        slider_objects.push((pos, h.start_time + time_offset));

                        ticks.reverse();
                        slider_objects.extend_from_slice(&ticks); // tick time doesn't need to be adjusted for some reason
                    }

                    // Handling last span separatly so that `ticks` vector isn't cloned again
                    let dist = ((*repeats - 1) % 2) as f32 * *pixel_len;
                    let time_offset = (duration / *repeats as f32) * (*repeats - 1) as f32;
                    let pos = curve.point_at_distance(dist);

                    slider_objects.push((pos, h.start_time + time_offset));

                    ticks.reverse();
                    slider_objects.append(&mut ticks); // automatically empties buffer for next slider
                }

                // Slider tail
                let dist_end = (*repeats % 2) as f32 * *pixel_len;
                let pos = curve.point_at_distance(dist_end);
                slider_objects.push((pos, h.start_time + duration));

                fruits += 1 + *repeats;
                droplets += slider_objects.len() - 1 - *repeats;

                let iter = slider_objects.into_iter().map(CatchObject::new);

                Some(Some(FruitOrJuice::Juice(iter)))
            }
            HitObjectKind::Spinner { .. } | HitObjectKind::Hold { .. } => Some(None),
        })
        .filter_map(identity)
        .flatten();

    // Hyper dash business
    let half_catcher_width = calculate_catch_width(attributes.cs) / 2.0 / ALLOWED_CATCH_RANGE;
    let mut last_direction = 0;
    let mut last_excess = half_catcher_width;

    // Strain business
    let mut movement = Movement::new(attributes.cs);
    let section_len = SECTION_LENGTH * attributes.clock_rate;
    let mut current_section_end =
        (map.hit_objects[0].start_time / section_len).ceil() * section_len;

    let mut prev = hit_objects.next().unwrap();
    let mut curr = hit_objects.next().unwrap();

    prev.init_hyper_dash(
        half_catcher_width,
        &curr,
        &mut last_direction,
        &mut last_excess,
    );

    for next in hit_objects {
        curr.init_hyper_dash(
            half_catcher_width,
            &next,
            &mut last_direction,
            &mut last_excess,
        );

        let h = DifficultyObject::new(
            &curr,
            &prev,
            movement.half_catcher_width,
            attributes.clock_rate,
        );

        while h.base.time > current_section_end {
            movement.save_current_peak();
            movement.start_new_section_from(current_section_end);
            current_section_end += section_len;
        }

        movement.process(&h);

        prev = curr;
        curr = next;
    }

    // Same as in loop but without init_hyper_dash because `curr` is the last element
    let h = DifficultyObject::new(
        &curr,
        &prev,
        movement.half_catcher_width,
        attributes.clock_rate,
    );

    while h.base.time > current_section_end {
        movement.save_current_peak();
        movement.start_new_section_from(current_section_end);

        current_section_end += section_len;
    }

    movement.process(&h);
    movement.save_current_peak();

    let stars = movement.difficulty_value().sqrt() * STAR_SCALING_FACTOR;

    DifficultyAttributes {
        stars,
        n_fruits: fruits,
        n_droplets: droplets,
        max_combo: fruits + droplets,
    }
}

#[inline]
pub(crate) fn calculate_catch_width(cs: f32) -> f32 {
    let scale = 1.0 - 0.7 * (cs - 5.0) / 5.0;

    CATCHER_SIZE * scale.abs() * ALLOWED_CATCH_RANGE
}

enum FruitOrJuice<I> {
    Fruit(Option<CatchObject>),
    Juice(I),
}

impl<I: Iterator<Item = CatchObject>> Iterator for FruitOrJuice<I> {
    type Item = CatchObject;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Fruit(fruit) => fruit.take(),
            Self::Juice(slider) => slider.next(),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Fruit(Some(_)) => (1, Some(1)),
            Self::Fruit(None) => (0, Some(0)),
            Self::Juice(slider) => slider.size_hint(),
        }
    }
}

#[derive(Default)]
pub struct DifficultyAttributes {
    pub stars: f32,
    pub max_combo: usize,
    pub n_fruits: usize,
    pub n_droplets: usize,
}

pub struct PpResult {
    pub pp: f32,
    pub stars: f32,
}

pub trait PpProvider {
    fn pp(&self) -> PpCalculator;
}

impl PpProvider for Beatmap {
    fn pp(&self) -> PpCalculator {
        PpCalculator::new(self)
    }
}

// TODO: Allow partial plays
pub struct PpCalculator<'m> {
    map: &'m Beatmap,
    attributes: Option<DifficultyAttributes>,
    mods: u32,
    combo: Option<usize>,

    n_fruits: Option<usize>,
    n_droplets: Option<usize>,
    n_tiny_droplets: Option<usize>,
    n_tiny_droplet_misses: Option<usize>,
    n_misses: usize,
}

impl<'m> PpCalculator<'m> {
    pub fn new(map: &'m Beatmap) -> Self {
        Self {
            map,
            attributes: None,
            mods: 0,
            combo: None,

            n_fruits: None,
            n_droplets: None,
            n_tiny_droplets: None,
            n_tiny_droplet_misses: None,
            n_misses: 0,
        }
    }

    pub fn attributes(mut self, attributes: DifficultyAttributes) -> Self {
        self.attributes.replace(attributes);

        self
    }

    pub fn mods(mut self, mods: u32) -> Self {
        self.mods = mods;

        self
    }

    pub fn combo(mut self, combo: usize) -> Self {
        self.combo.replace(combo);

        self
    }

    pub fn fruits(mut self, n_fruits: usize) -> Self {
        self.n_fruits.replace(n_fruits);

        self
    }

    pub fn droplets(mut self, n_droplets: usize) -> Self {
        self.n_droplets.replace(n_droplets);

        self
    }

    pub fn tiny_droplets(mut self, n_tiny_droplets: usize) -> Self {
        self.n_tiny_droplets.replace(n_tiny_droplets);

        self
    }

    pub fn tiny_droplet_misses(mut self, n_tiny_droplet_misses: usize) -> Self {
        self.n_tiny_droplet_misses.replace(n_tiny_droplet_misses);

        self
    }

    pub fn misses(mut self, n_misses: usize) -> Self {
        self.n_misses = n_misses;

        self
    }

    /// Generate the hit results with respect to the given accuracy between `0` and `100`.
    ///
    /// Be sure to set `misses` beforehand! Also, if available, set `attributes` beforehand.
    pub fn accuracy(mut self, acc: f32) -> Self {
        if self.attributes.is_none() {
            self.attributes.replace(stars(self.map, self.mods));
        }

        let attributes = self.attributes.as_ref().unwrap();

        let n_droplets = self
            .n_droplets
            .unwrap_or_else(|| attributes.n_droplets.saturating_sub(self.n_misses));

        let n_fruits = self.n_fruits.unwrap_or_else(|| {
            attributes
                .max_combo
                .saturating_sub(self.n_misses.saturating_sub(n_droplets))
        });

        let max_tiny_droplets = 0; // TODO

        let n_tiny_droplets = self.n_tiny_droplets.unwrap_or_else(|| {
            ((acc * (attributes.max_combo + max_tiny_droplets) as f32).round() as usize)
                .saturating_sub(n_fruits)
                .saturating_sub(n_droplets)
        });

        let n_tiny_droplet_misses = max_tiny_droplets - n_tiny_droplets;

        self.n_fruits.replace(n_fruits);
        self.n_droplets.replace(n_droplets);
        self.n_tiny_droplets.replace(n_tiny_droplets);
        self.n_tiny_droplet_misses.replace(n_tiny_droplet_misses);

        self
    }

    pub fn calculate(mut self) -> PpResult {
        let attributes = self
            .attributes
            .take()
            .unwrap_or_else(|| stars(self.map, self.mods));

        let stars = attributes.stars;

        // Relying heavily on aim
        let mut pp = (5.0 * ((stars / 0.0049).max(1.0)) - 4.0).powi(2) / 100_000.0;

        let mut combo_hits = self.combo_hits();
        if combo_hits == 0 {
            combo_hits = attributes.max_combo;
        }

        // Longer maps are worth more
        let len_bonus = 0.95
            + 0.3 * (combo_hits as f32 / 2500.0).min(1.0)
            + (combo_hits > 2500) as u8 as f32 * (combo_hits as f32 / 2500.0).log10() * 0.475;
        pp *= len_bonus;

        // Penalize misses exponentially
        pp *= 0.97_f32.powi(self.n_misses as i32);

        // Combo scaling
        if let Some(combo) = self.combo.filter(|_| attributes.max_combo > 0) {
            pp *= (combo as f32 / attributes.max_combo as f32)
                .powf(0.8)
                .min(1.0);
        }

        // AR scaling
        let ar = self.map.ar;
        let mut ar_factor = 1.0;
        if ar > 9.0 {
            ar_factor += 0.1 * (ar - 9.0) + (ar > 10.0) as u8 as f32 * 0.1 * (ar - 10.0);
        } else if ar < 8.0 {
            ar_factor += 0.025 * (8.0 - ar);
        }
        pp *= ar_factor;

        // HD bonus
        if self.mods.hd() {
            if ar <= 10.0 {
                pp *= 1.05 + 0.075 * (10.0 - ar);
            } else if ar > 10.0 {
                pp *= 1.01 + 0.04 * (11.0 - ar.min(11.0));
            }
        }

        // FL bonus
        if self.mods.fl() {
            pp *= 1.35 * len_bonus;
        }

        // Accuracy scaling
        pp *= self.acc().powf(5.5);

        // NF penalty
        if self.mods.nf() {
            pp *= 0.9;
        }

        PpResult { pp, stars }
    }

    #[inline]
    fn combo_hits(&self) -> usize {
        self.n_fruits.unwrap_or(0) + self.n_droplets.unwrap_or(0) + self.n_misses
    }

    #[inline]
    fn successful_hits(&self) -> usize {
        self.n_fruits.unwrap_or(0)
            + self.n_droplets.unwrap_or(0)
            + self.n_tiny_droplets.unwrap_or(0)
    }

    #[inline]
    fn total_hits(&self) -> usize {
        self.successful_hits() + self.n_tiny_droplet_misses.unwrap_or(0) + self.n_misses
    }

    #[inline]
    fn acc(&self) -> f32 {
        let total_hits = self.total_hits();

        if total_hits == 0 {
            0.0
        } else {
            (self.successful_hits() as f32 / total_hits as f32)
                .max(0.0)
                .min(1.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn test_single() {
        let map_id = 1972149;
        let file = match File::open(format!("E:/Games/osu!/beatmaps/{}.osu", map_id)) {
            Ok(file) => file,
            Err(why) => panic!("Could not open file: {}", why),
        };
        // let file = match File::open(format!("E:/Games/osu!/beatmaps/{}.osu", map_id)) {
        //     Ok(file) => file,
        //     Err(why) => panic!("Could not open file: {}", why),
        // };

        let map = match Beatmap::parse(file) {
            Ok(map) => map,
            Err(why) => panic!("Error while parsing map: {}", why),
        };

        let mods = 0;
        let stars = stars(&map, mods).stars;

        println!("Stars: {} [map={} | mods={}]", stars, map_id, mods);
    }

    #[test]
    fn test_fruits() {
        let margin = 0.005;

        #[rustfmt::skip]
        let data = vec![
            (1977380, 1 << 8, 2.0564713386286573),// HT
            (1977380, 0, 2.5695489769068742),     // NM
            (1977380, 1 << 6, 3.589887228221038), // DT
            (1977380, 1 << 4, 3.1515873669521928),// HR
            (1977380, 1 << 1, 3.0035260129778396),// EZ

            (1974968, 1 << 8, 1.9544305373156605),// HT
            (1974968, 0, 2.521701539665241),      // NM
            (1974968, 1 << 6, 3.650649037957456), // DT
            (1974968, 1 << 4, 3.566302788963401), // HR
            (1974968, 1 << 1, 2.2029392066882654),// EZ

            (2420076, 1 << 8, 4.791039358886245), // HT
            (2420076, 0, 6.223136555625056),      // NM
            (2420076, 1 << 6, 8.908315960310958), // DT
            (2420076, 1 << 4, 6.54788067620051),  // HR
            (2420076, 1 << 1, 6.067971540209479), // EZ

            (2206596, 1 << 8, 4.767182611189798), // HT
            (2206596, 0, 6.157660207091584),      // NM
            (2206596, 1 << 6, 8.93391286552717),  // DT
            (2206596, 1 << 4, 6.8639096665110735),// HR
            (2206596, 1 << 1, 5.60279198088948),  // EZ

            // Super long juice stream towards end
            // (1972149, 1 << 8, 4.671425766413811), // HT
            // (1972149, 0, 6.043742871084152),      // NM
            // (1972149, 1 << 6, 8.469259368304225), // DT
            // (1972149, 1 << 4, 6.81222485322862),  // HR
            // (1972149, 1 << 1, 5.289343020686747), // EZ

            // Convert slider fiesta
            // (1657535, 1 << 8, 3.862453635711741), // HT
            // (1657535, 0, 4.792543335869686),      // NM
            // (1657535, 1 << 6, 6.655478646330863), // DT
            // (1657535, 1 << 4, 5.259728567781568), // HR
            // (1657535, 1 << 1, 4.127535166776765), // EZ
        ];

        for (map_id, mods, expected_stars) in data {
            let file = match File::open(format!("./test/{}.osu", map_id)) {
                Ok(file) => file,
                Err(why) => panic!("Could not open file {}.osu: {}", map_id, why),
            };

            let map = match Beatmap::parse(file) {
                Ok(map) => map,
                Err(why) => panic!("Error while parsing map {}: {}", map_id, why),
            };

            let stars = stars(&map, mods).stars;

            assert!(
                (stars - expected_stars).abs() < margin,
                "Stars: {} | Expected: {} => {} margin [map {} | mods {}]",
                stars,
                expected_stars,
                (stars - expected_stars).abs(),
                map_id,
                mods
            );
        }
    }
}