use neuromita::games::seabattle::{Coordinate, Orientation, SeaBattle, Side};

// Expected results independently captured from Python 36f942106ab04522492477087e7db5ca41f0d626.
#[test]
fn full_match_matches_the_current_python_reference() {
    let mut game = SeaBattle::new();
    for side in [Side::Player, Side::Mita] {
        for (coordinate, length) in [
            ("A1", 4),
            ("F1", 3),
            ("A3", 3),
            ("E3", 2),
            ("H3", 2),
            ("A5", 2),
            ("D5", 1),
            ("F5", 1),
            ("H5", 1),
            ("J5", 1),
        ] {
            game.place(
                side,
                Coordinate::parse(coordinate).unwrap(),
                length,
                Orientation::Horizontal,
            )
            .unwrap();
        }
    }
    let expected = [
        (1, "J10", "not_your_turn", 0, "battle", None),
        (0, "J10", "miss", 1, "battle", None),
        (0, "A1", "not_your_turn", 1, "battle", None),
        (1, "J9", "miss", 0, "battle", None),
        (0, "A1", "hit", 0, "battle", None),
        (0, "A1", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "B1", "hit", 0, "battle", None),
        (0, "B1", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "C1", "hit", 0, "battle", None),
        (0, "C1", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "D1", "sunk", 0, "battle", None),
        (0, "D1", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "F1", "hit", 0, "battle", None),
        (0, "F1", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "G1", "hit", 0, "battle", None),
        (0, "G1", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "H1", "sunk", 0, "battle", None),
        (0, "H1", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "A3", "hit", 0, "battle", None),
        (0, "A3", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "B3", "hit", 0, "battle", None),
        (0, "B3", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "C3", "sunk", 0, "battle", None),
        (0, "C3", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "E3", "hit", 0, "battle", None),
        (0, "E3", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "F3", "sunk", 0, "battle", None),
        (0, "F3", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "H3", "hit", 0, "battle", None),
        (0, "H3", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "I3", "sunk", 0, "battle", None),
        (0, "I3", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "A5", "hit", 0, "battle", None),
        (0, "A5", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "B5", "sunk", 0, "battle", None),
        (0, "B5", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "D5", "sunk", 0, "battle", None),
        (0, "D5", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "F5", "sunk", 0, "battle", None),
        (0, "F5", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "H5", "sunk", 0, "battle", None),
        (0, "H5", "already_shot", 0, "battle", None),
        (1, "J8", "not_your_turn", 0, "battle", None),
        (0, "J5", "win", 0, "game_over", Some(0)),
        (0, "J8", "invalid_phase", 0, "game_over", Some(0)),
    ];
    for (actor, coordinate, expected_result, turn, phase, winner) in expected {
        let before_revision = game.revision();
        let before_player = game.own_board_text(Side::Player);
        let before_mita = game.own_board_text(Side::Mita);
        let result = game.shot(
            Side::from_index(actor).unwrap(),
            Coordinate::parse(coordinate).unwrap(),
        );
        let actual = match result {
            Ok(result) => result.code(),
            Err(error) => error.code(),
        };
        assert_eq!(
            actual, expected_result,
            "actor={actor}, coordinate={coordinate}"
        );
        assert_eq!(game.turn().index(), turn);
        assert_eq!(game.phase().code(), phase);
        assert_eq!(game.winner().map(Side::index), winner);
        if result.is_err() {
            assert_eq!(game.revision(), before_revision);
            assert_eq!(game.own_board_text(Side::Player), before_player);
            assert_eq!(game.own_board_text(Side::Mita), before_mita);
        }
    }
}

#[test]
fn coordinate_grammar_rejects_signs_unicode_and_non_cells() {
    for bad in ["A+1", "A-1", "A0", "A11", "K1", "А1", "A１", "A1junk"] {
        assert!(Coordinate::parse(bad).is_none(), "{bad}");
    }
}

#[test]
fn tactical_hints_depend_only_on_observed_hits_not_hidden_ship_identity() {
    let first = [
        ("A1", 4),
        ("F1", 3),
        ("A3", 3),
        ("E3", 2),
        ("H3", 2),
        ("A5", 2),
        ("D5", 1),
        ("F5", 1),
        ("H5", 1),
        ("J5", 1),
    ];
    let second = [
        ("A1", 2),
        ("D1", 2),
        ("A3", 4),
        ("F3", 3),
        ("A5", 3),
        ("E5", 2),
        ("A7", 1),
        ("C7", 1),
        ("E7", 1),
        ("G7", 1),
    ];
    let make = |defender: &[(&str, u8)]| {
        let mut game = SeaBattle::new();
        for (side, layout) in [(Side::Player, first.as_slice()), (Side::Mita, defender)] {
            for (coordinate, length) in layout {
                game.place(
                    side,
                    Coordinate::parse(coordinate).unwrap(),
                    *length,
                    Orientation::Horizontal,
                )
                .unwrap();
            }
        }
        for coordinate in ["A1", "D1"] {
            assert_eq!(
                game.shot(Side::Player, Coordinate::parse(coordinate).unwrap())
                    .unwrap()
                    .code(),
                "hit"
            );
        }
        game
    };
    let a = make(&first);
    let b = make(&second);
    assert_eq!(
        a.opponent_board_text(Side::Player),
        b.opponent_board_text(Side::Player)
    );
    assert_eq!(a.wounded(Side::Player), b.wounded(Side::Player));
    assert_eq!(a.hunt_targets(Side::Player), b.hunt_targets(Side::Player));
}
