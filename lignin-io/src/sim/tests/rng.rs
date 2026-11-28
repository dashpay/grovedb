use super::*;

#[test]
fn the_generator_is_splitmix64() {
    let mut rng = SimRng::new(1_234_567);
    let expected = [
        6_457_827_717_110_365_317,
        3_203_168_211_198_807_973,
        9_817_491_932_198_370_423,
        4_593_380_528_125_082_431,
        16_408_922_859_458_223_821,
    ];
    assert_eq!(expected.map(|_| rng.next_u64()), expected);

    let mut rng = SimRng::new(0);
    let expected = [
        0xE220_A839_7B1D_CDAF,
        0x6E78_9E6A_A1B9_65F4,
        0x06C4_5D18_8009_454F,
    ];
    assert_eq!(expected.map(|_| rng.next_u64()), expected);
}

#[test]
fn a_derived_generator_depends_on_seed_stream_and_index_only() {
    let first = |seed, stream, index| SimRng::derive(seed, stream, index).next_u64();
    assert_eq!(first(1, 2, 3), first(1, 2, 3));
    assert_eq!(SimRng::derive(1, 2, 3), SimRng::derive(1, 2, 3));

    let numbers = [
        first(1, 2, 3),
        first(2, 2, 3),
        first(1, 3, 3),
        first(1, 2, 4),
        first(1, 3, 2),
    ];
    let distinct: BTreeSet<u64> = numbers.into_iter().collect();
    assert_eq!(distinct.len(), numbers.len());
    assert_eq!(
        [first(0, 0, 0), first(1, 2, 3), first(u64::MAX, 6, 1 << 40)],
        [
            0x2382_75BC_38FC_BE91,
            0xD073_4750_FDE3_62B3,
            0xDD22_A930_3837_A2CF
        ]
    );
}

#[test]
fn below_stays_below_and_reaches_every_number() {
    let mut rng = SimRng::new(7);
    assert!((0..100).all(|_| rng.below(1) == 0));

    let mut seen = [0u32; 6];
    for _ in 0..6_000 {
        seen[rng.below(6) as usize] += 1;
    }
    assert!(seen.iter().all(|count| (800..1_200).contains(count)));

    let wide = (1 << 63) + 1;
    assert!((0..200).all(|_| rng.below(wide) < wide));
    assert!((0..200).any(|_| rng.below(u64::MAX) > u64::MAX / 2));
}

#[test]
fn below_is_the_same_on_every_machine() {
    let mut rng = SimRng::new(42);
    let drawn = [10, 1_000, 1 << 40, (1 << 63) + 1, u64::MAX].map(|bound| rng.below(bound));
    assert_eq!(
        drawn,
        [
            7,
            159,
            306_325_182_227,
            8_007_990_562_831_494_531,
            4_028_864_712_777_624_924
        ]
    );
}

#[test]
#[should_panic(expected = "no number below 0")]
fn below_needs_a_bound() {
    SimRng::new(1).below(0);
}

#[test]
fn a_chance_happens_as_often_as_it_says() {
    let mut rng = SimRng::new(9);
    let before = rng.clone();
    assert!(!rng.chance(Chance::NEVER));
    assert!(rng.chance(Chance::ALWAYS));
    assert!(rng.chance(Chance::new(5, 5)));
    assert_eq!(rng, before);

    let mut rng = SimRng::new(10);
    let mut count = |chance| (0..8_000).filter(|_| rng.chance(chance)).count();
    assert!((1_800..2_200).contains(&count(Chance::new(1, 4))));
    assert!((40..130).contains(&count(Chance::one_in(100))));

    assert_eq!(format!("{:?}", Chance::new(3, 8)), "3/8");
    assert_eq!(Chance::one_in(8), Chance::new(1, 8));
}

#[test]
#[should_panic(expected = "between 0 and 1")]
fn a_chance_is_at_most_one() {
    let _ = Chance::new(2, 1);
}

#[test]
#[should_panic(expected = "between 0 and 1")]
fn a_chance_needs_a_denominator() {
    let _ = Chance::one_in(0);
}
