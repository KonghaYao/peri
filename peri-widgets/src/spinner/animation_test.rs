    #[test]
    fn test_tick_to_frame_cycle() {
        for i in 0..20 {
            let frame = tick_to_frame(i);
            assert!(
                BRAILLE_FRAMES.contains(&frame),
                "tick {} returned {:?} not in BRAILLE_FRAMES",
                i,
                frame
            );
        }
    }

    #[test]
    fn test_format_elapsed() {
        assert_eq!(format_elapsed(90_000), "1m 30s");
        assert_eq!(format_elapsed(30_000), "30s");
        assert_eq!(format_elapsed(5_000), "5s");
    }

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5k");
        assert_eq!(format_tokens(2200), "2.2k");
        assert_eq!(format_tokens(15000), "15k");
        assert_eq!(format_tokens(0), "0");
    }
