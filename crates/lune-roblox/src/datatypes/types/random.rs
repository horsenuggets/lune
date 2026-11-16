use core::{convert::Infallible, fmt};

use glam::Vec3;
use mlua::prelude::*;

use lune_utils::TableBuilder;

use crate::exports::LuaExportsTable;

use super::{super::*, Vector3};

const MAX_SAFE_SEED: f64 = 9_007_199_254_740_991.0;
const PCG32_INCREMENT: u64 = 105;
const PCG32_MULTIPLIER: u64 = 6_364_136_223_846_793_005;
const SHUFFLE_NIL_ERROR: &str = "Attempted to shuffle table containing nil values. \
                                 This is not allowed because the table length may change.";
const TYPE_NAME: &str = "Random";

/**
    An implementation of the [Random](https://create.roblox.com/docs/reference/engine/datatypes/Random) Roblox datatype.

    This implements all documented methods & constructors of the `Random` class as of
    November 2026. Generators produce the same values as Roblox for the same seed, since
    they share its PCG32 (XSH-RR) algorithm, seeding, and sampling behavior.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Random {
    state: u64,
}

impl Random {
    /**
        Creates a generator from a seed, truncating it toward zero the way Roblox does.
        Seeds outside of the safe integer range, including NaN and infinities, become zero.
    */
    #[must_use]
    pub fn new(seed: f64) -> Self {
        let seed = seed.trunc();
        if seed.abs() <= MAX_SAFE_SEED {
            Self::from_seed_bits(seed as i64 as u64)
        } else {
            Self::from_seed_bits(0)
        }
    }

    fn from_seed_bits(seed: u64) -> Self {
        let mut random = Self { state: 0 };
        random.next_u32();
        random.state = random.state.wrapping_add(seed);
        random.next_u32();
        random
    }

    fn next_u32(&mut self) -> u32 {
        let state = self.state;
        self.state = state
            .wrapping_mul(PCG32_MULTIPLIER)
            .wrapping_add(PCG32_INCREMENT);
        let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
        xorshifted.rotate_right((state >> 59) as u32)
    }

    fn next_u64(&mut self) -> u64 {
        let low = u64::from(self.next_u32());
        let high = u64::from(self.next_u32());
        low | (high << 32)
    }

    /**
        Returns a number uniformly distributed over `[0, 1]`.
    */
    pub fn next_number(&mut self) -> f64 {
        self.next_u64() as f64 * 2f64.powi(-64)
    }

    /**
        Returns a number uniformly distributed between `min` and `max`, swapping them
        first if `min` is greater than `max`.
    */
    pub fn next_number_between(&mut self, min: f64, max: f64) -> f64 {
        let (min, max) = if min > max { (max, min) } else { (min, max) };
        let fraction = self.next_number();
        min * (1.0 - fraction) + max * fraction
    }

    /**
        Returns an integer uniformly distributed over `[min, max]`, swapping them first if
        `min` is greater than `max`.

        Bounds are truncated toward zero and saturated to the 64-bit integer range, with NaN
        becoming zero. Spans that fit in 32 bits consume one 32-bit draw, and larger spans
        consume a 64-bit draw, matching Roblox.
    */
    pub fn next_integer(&mut self, min: f64, max: f64) -> f64 {
        self.next_integer_between(min as i64, max as i64) as f64
    }

    fn next_integer_between(&mut self, min: i64, max: i64) -> i64 {
        let (min, max) = if min > max { (max, min) } else { (min, max) };
        let span = max.abs_diff(min) as u128 + 1;
        let offset = if span < u128::from(u32::MAX) {
            (span * u128::from(self.next_u32())) >> 32
        } else if span > u128::from(u64::MAX) {
            // The full 64-bit range uses the raw bits without an offset
            return self.next_u64() as i64;
        } else {
            u128::from(multiply_high(span as u64, self.next_u64()))
        };
        (i128::from(min) + offset as i128) as i64
    }

    /**
        Returns a unit vector with a direction uniformly distributed over the unit sphere.
    */
    pub fn next_unit_vector(&mut self) -> Vec3 {
        let scale = 2f32.powi(-32);
        let theta = self.next_u32() as f32 * scale * core::f32::consts::TAU;
        let z = self.next_u32() as f32 * scale * 2.0 - 1.0;
        let radius = (1.0 - z * z).sqrt();
        let theta = f64::from(theta);
        Vec3::new(radius * theta.cos() as f32, radius * theta.sin() as f32, z)
    }

    /**
        Shuffles items in place with a Fisher-Yates shuffle, drawing indices the same way
        Roblox does so the order matches for the same seed.
    */
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        self.shuffle_with(items.len(), |upper, picked| {
            items.swap(upper - 1, picked - 1);
            Ok::<_, Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
    }

    /**
        Runs a Fisher-Yates shuffle over `length` items by calling `swap` with the 1-based
        positions to exchange, so callers can shuffle storage they can't borrow as a slice.
    */
    fn shuffle_with<E>(
        &mut self,
        length: usize,
        mut swap: impl FnMut(usize, usize) -> Result<(), E>,
    ) -> Result<(), E> {
        for upper in (2..=length).rev() {
            let picked = self.next_integer_between(1, upper as i64) as usize;
            swap(upper, picked)?;
        }
        Ok(())
    }
}

/**
    Computes the high 64 bits of a 64-bit product from its 32-bit partial products,
    dropping the low cross term and carries exactly like Roblox does.
*/
fn multiply_high(lhs: u64, rhs: u64) -> u64 {
    let (lhs_high, lhs_low) = (lhs >> 32, lhs & 0xFFFF_FFFF);
    let (rhs_high, rhs_low) = (rhs >> 32, rhs & 0xFFFF_FFFF);
    (lhs_high * rhs_high)
        .wrapping_add((lhs_high * rhs_low) >> 32)
        .wrapping_add((lhs_low * rhs_high) >> 32)
}

fn type_name_of(value: &LuaValue) -> String {
    match value {
        LuaValue::UserData(userdata) => userdata
            .type_name()
            .map_or_else(|_| "userdata".to_string(), |name| name.to_string_lossy()),
        LuaValue::Integer(_) => "number".to_string(),
        LuaValue::LightUserData(_) => "userdata".to_string(),
        value => value.type_name().to_string(),
    }
}

fn argument_error(
    position: usize,
    function: Option<&str>,
    expected: &str,
    got: Option<&LuaValue>,
) -> LuaError {
    let function = function.map_or_else(String::new, |name| format!(" to '{name}'"));
    LuaError::runtime(match got {
        Some(value) => format!(
            "invalid argument #{position}{function} ({expected} expected, got {})",
            type_name_of(value)
        ),
        None => format!("missing argument #{position}{function} ({expected} expected)"),
    })
}

fn number_argument(
    lua: &Lua,
    args: &LuaMultiValue,
    position: usize,
    function: Option<&str>,
) -> LuaResult<f64> {
    let value = args.get(position - 1);
    match value {
        Some(inner) => lua.coerce_number(inner.clone())?,
        None => None,
    }
    .ok_or_else(|| argument_error(position, function, "number", value))
}

fn this_argument(args: &LuaMultiValue, method: &str) -> LuaResult<LuaUserDataRefMut<Random>> {
    let value = args.front();
    if let Some(LuaValue::UserData(userdata)) = value
        && let Ok(this) = userdata.borrow_mut::<Random>()
    {
        return Ok(this);
    }
    Err(argument_error(1, Some(method), TYPE_NAME, value))
}

fn member_error(key: &LuaValue) -> LuaError {
    let key = key.to_string().unwrap_or_else(|_| type_name_of(key));
    LuaError::runtime(format!("{key} is not a valid member of {TYPE_NAME}"))
}

impl LuaExportsTable for Random {
    const EXPORT_NAME: &'static str = TYPE_NAME;

    fn create_exports_table(lua: Lua) -> LuaResult<LuaTable> {
        let random_new = |lua: &Lua, args: LuaMultiValue| match args.front() {
            None | Some(LuaValue::Nil) => Ok(Random::from_seed_bits(rand::random())),
            Some(_) => Ok(Random::new(number_argument(lua, &args, 1, Some("new"))?)),
        };

        TableBuilder::new(lua)?
            .with_function("new", random_new)?
            .build_readonly()
    }
}

impl LuaUserData for Random {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_function("Clone", |_, args: LuaMultiValue| {
            Ok(*this_argument(&args, "Clone")?)
        });
        methods.add_function("NextInteger", |lua, args: LuaMultiValue| {
            let mut this = this_argument(&args, "NextInteger")?;
            let min = number_argument(lua, &args, 2, None)?;
            let max = number_argument(lua, &args, 3, None)?;
            Ok(this.next_integer(min, max))
        });
        methods.add_function("NextNumber", |lua, args: LuaMultiValue| {
            let mut this = this_argument(&args, "NextNumber")?;
            match args.len() {
                1 => Ok(this.next_number()),
                2 => Err(LuaError::runtime(format!(
                    "{TYPE_NAME}:NextNumber requires 0 or 2 arguments"
                ))),
                _ => {
                    let min = number_argument(lua, &args, 2, None)?;
                    let max = number_argument(lua, &args, 3, None)?;
                    Ok(this.next_number_between(min, max))
                }
            }
        });
        methods.add_function("NextUnitVector", |_, args: LuaMultiValue| {
            let mut this = this_argument(&args, "NextUnitVector")?;
            Ok(Vector3(this.next_unit_vector()))
        });
        methods.add_function("Shuffle", |_, args: LuaMultiValue| {
            let mut this = this_argument(&args, "Shuffle")?;
            let Some(LuaValue::Table(table)) = args.get(1) else {
                return Err(argument_error(2, None, "table", args.get(1)));
            };
            let length = table.raw_len();
            for index in 1..=length {
                if table.raw_get::<LuaValue>(index)?.is_nil() {
                    return Err(LuaError::runtime(SHUFFLE_NIL_ERROR));
                }
            }
            if length < 2 {
                return Ok(());
            }
            if table.is_readonly() {
                return Err(LuaError::runtime("attempt to modify a readonly table"));
            }
            this.shuffle_with(length, |upper, picked| {
                let upper_value: LuaValue = table.raw_get(upper)?;
                let picked_value: LuaValue = table.raw_get(picked)?;
                table.raw_set(upper, picked_value)?;
                table.raw_set(picked, upper_value)
            })
        });

        methods.add_meta_function(
            LuaMetaMethod::Index,
            |_, (_, key): (LuaValue, LuaValue)| -> LuaResult<()> { Err(member_error(&key)) },
        );
        methods.add_meta_function(
            LuaMetaMethod::NewIndex,
            |_, (_, key, _): (LuaValue, LuaValue, LuaValue)| -> LuaResult<()> {
                Err(member_error(&key))
            },
        );
        methods.add_meta_method(LuaMetaMethod::Eq, userdata_impl_eq);
        methods.add_meta_method(LuaMetaMethod::ToString, userdata_impl_to_string);
    }
}

impl fmt::Display for Random {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{TYPE_NAME}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(values: &[f64]) -> Vec<u64> {
        values.iter().map(|value| value.to_bits()).collect()
    }

    #[test]
    fn next_number_matches_roblox() {
        let mut random = Random::new(123.0);
        let values = [
            random.next_number(),
            random.next_number(),
            random.next_number(),
        ];
        assert_eq!(
            bits(&values),
            bits(&[
                0.557_634_200_277_247_1,
                0.567_774_387_777_16,
                0.932_357_652_636_514
            ])
        );
    }

    #[test]
    fn seeds_truncate_toward_zero_and_clamp_to_safe_integers() {
        let first = |seed: f64| Random::new(seed).next_number().to_bits();
        for seed in [
            0.99,
            -0.5,
            2f64.powi(53),
            -(2f64.powi(53)),
            f64::NAN,
            f64::INFINITY,
        ] {
            assert_eq!(
                first(seed),
                first(0.0),
                "Seed {seed} should behave like zero."
            );
        }
        assert_eq!(first(-1.5), first(-1.0));
        assert_eq!(first(-1.0), 0.003_715_568_824_263_543_5f64.to_bits());
        assert_eq!(
            first(2f64.powi(32) + 5.0),
            0.382_865_447_607_354_86f64.to_bits()
        );
        assert_eq!(first(MAX_SAFE_SEED), 0.027_684_032_073_451_197f64.to_bits());
        assert_eq!(first(-MAX_SAFE_SEED), 0.227_887_036_556_987_44f64.to_bits());
    }

    #[test]
    fn next_integer_matches_roblox_across_span_sizes() {
        // Each case uses a fresh generator with seed 1, captured from Roblox
        let cases: &[(f64, f64, f64)] = &[
            (0.0, 2f64.powi(31) - 1.0, 2062141372.0),
            (0.0, 2f64.powi(31), 2062141373.0),
            (-(2f64.powi(31)), 0.0, -85342275.0),
            (-(2f64.powi(31)), 2f64.powi(31) - 1.0, 1926011170.0),
            (0.0, 2f64.powi(32) - 2.0, 4073494817.0),
            (0.0, 2f64.powi(32) - 1.0, 4073494818.0),
            (0.0, 2f64.powi(32) + 1.0, 4073494819.0),
            (0.0, 2f64.powi(40), 1042814673653.0),
            (-(2f64.powi(53)), 2f64.powi(53), 8078276358403299.0),
            (0.0, 2f64.powi(60), 1.093_470_439_241_234_7e18),
            (-(2f64.powi(63)), 2f64.powi(63), -9.512_170_458_497_967e17),
            (f64::NEG_INFINITY, f64::INFINITY, -9.512_170_458_497_967e17),
            (f64::INFINITY, 1.0, 8.747_763_513_929_878e18),
            (f64::NEG_INFINITY, 1.0, -4.756_085_229_248_983_7e17),
            (f64::NAN, 1.0, 1.0),
            (f64::NAN, f64::NAN, 0.0),
            (5.0, 5.0, 5.0),
            (-3.0, -1.0, -1.0),
        ];
        for &(min, max, expected) in cases {
            let actual = Random::new(1.0).next_integer(min, max);
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "NextInteger({min}, {max})"
            );
        }
    }

    #[test]
    fn next_integer_switches_to_64_bit_draws_at_the_roblox_threshold() {
        let follow_up = |max: f64| {
            let mut random = Random::new(1.0);
            random.next_integer(0.0, max);
            random.next_number().to_bits()
        };
        let after_32_bit_draw = follow_up(1.0);
        assert_eq!(follow_up(4294967293.0), after_32_bit_draw);
        assert_ne!(follow_up(4294967294.0), after_32_bit_draw);
    }

    #[test]
    fn next_integer_truncates_and_swaps_bounds() {
        let mut random = Random::new(123.0);
        let values = [
            random.next_integer(1.0, 10.0),
            random.next_integer(-5.0, 5.0),
            random.next_integer(10.0, 1.0),
            random.next_integer(0.0, 0.0),
        ];
        assert_eq!(bits(&values), bits(&[4.0, 1.0, 1.0, 0.0]));
        let mut random = Random::new(123.0);
        let values = [
            random.next_integer(1.9, 5.9),
            random.next_integer(-1.5, 3.7),
        ];
        assert_eq!(bits(&values), bits(&[2.0, 1.0]));
    }

    #[test]
    fn next_number_between_matches_roblox() {
        let mut random = Random::new(123.0);
        let values = [
            random.next_number_between(5.0, 10.0),
            random.next_number_between(10.0, 5.0),
            random.next_number_between(-1.0, 1.0),
        ];
        assert_eq!(
            bits(&values),
            bits(&[
                7.788_171_001_386_235,
                7.838_871_938_885_8,
                0.864_715_305_273_028_1
            ])
        );
        let mut random = Random::new(-31675.0);
        random.shuffle(&mut [0; 6]);
        random.next_integer(-81_209_034_151.530_12, 43_315.170_794_840_626);
        let values = [
            random.next_number_between(-154_090.738_662_483_58, 346_021.923_093_861_6),
            random.next_number_between(-46_132.815_211_116_12, 245_815.286_768_707_13),
        ];
        assert_eq!(
            bits(&values),
            bits(&[-101355.3473936246, 10643.594664331038])
        );
        assert!(Random::new(7.0).next_number_between(f64::NAN, 0.0).is_nan());
        assert!(
            Random::new(7.0)
                .next_number_between(f64::NEG_INFINITY, f64::INFINITY)
                .is_nan()
        );
        assert_eq!(
            Random::new(7.0)
                .next_number_between(0.0, f64::INFINITY)
                .to_bits(),
            f64::INFINITY.to_bits()
        );
    }

    #[test]
    fn next_unit_vector_matches_roblox() {
        let mut random = Random::new(123.0);
        let vectors = [random.next_unit_vector(), random.next_unit_vector()];
        let expected = [
            Vec3::new(-0.433_586_93, 0.893_708_94, 0.115_268_35),
            Vec3::new(0.989_070_65, 0.058_014_836, 0.135_548_83),
        ];
        for (actual, expected) in vectors.iter().zip(expected) {
            assert_eq!(
                actual.to_array().map(f32::to_bits),
                expected.to_array().map(f32::to_bits)
            );
        }
    }

    #[test]
    fn shuffle_matches_roblox() {
        let mut random = Random::new(123.0);
        let mut items: Vec<u32> = (1..=10).collect();
        random.shuffle(&mut items);
        assert_eq!(items, [7, 3, 9, 8, 5, 2, 10, 1, 6, 4]);
        assert_eq!(
            random.next_number().to_bits(),
            0.806_651_579_264_363_8f64.to_bits()
        );
    }

    #[test]
    fn shuffle_skips_draws_for_short_slices() {
        for length in 0..=1 {
            let mut random = Random::new(1.0);
            random.shuffle(&mut vec![0; length]);
            assert_eq!(random, Random::new(1.0));
        }
    }

    #[test]
    fn clones_share_state_and_advance_independently() {
        let mut original = Random::new(42.0);
        let mut clone = original;
        assert_eq!(original, clone);
        assert_eq!(
            original.next_number().to_bits(),
            clone.next_number().to_bits()
        );
        original.next_number();
        assert_ne!(original, clone);
    }

    #[test]
    fn generation_is_deterministic() {
        let sample = || {
            let mut random = Random::new(99.0);
            (
                random.next_number().to_bits(),
                random.next_integer(-1e12, 1e12).to_bits(),
                random.next_unit_vector().to_array().map(f32::to_bits),
            )
        };
        assert_eq!(sample(), sample());
    }
}
