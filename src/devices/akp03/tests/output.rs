use super::super::output::{
    Command, PacketError, REPORT_LEN, REPORT_PAYLOAD_LEN, command_report, frame_jpeg,
};

fn marker_image(length: usize) -> Vec<u8> {
    let mut image = (0..length)
        .map(|index| (index.wrapping_mul(31) & 0xff) as u8)
        .collect::<Vec<_>>();
    if length >= 4 {
        image[..2].copy_from_slice(&[0xff, 0xd8]);
        image[length - 2..].copy_from_slice(&[0xff, 0xd9]);
    }
    image
}

#[test]
fn all_commands_have_exact_zero_filled_skeletons_and_bounds() {
    for (command, opcode) in [
        (Command::Initialize, b"DIS".as_slice()),
        (Command::Brightness(0), b"LIG".as_slice()),
        (Command::Brightness(50), b"LIG".as_slice()),
        (Command::Brightness(100), b"LIG".as_slice()),
        (Command::KeepAlive, b"CONNECT".as_slice()),
        (Command::Flush, b"STP".as_slice()),
        (Command::ClearVolatileScene, b"CLE".as_slice()),
    ] {
        let report = command_report(command).unwrap();
        let mut expected = [0; REPORT_LEN];
        expected[1..4].copy_from_slice(b"CRT");
        expected[6..6 + opcode.len()].copy_from_slice(opcode);
        match command {
            Command::Brightness(percent) => expected[11] = percent,
            Command::ClearVolatileScene => expected[12] = 0xff,
            _ => {}
        }
        assert_eq!(report, expected, "{command:?}");
    }
    for percent in [101, 255] {
        assert_eq!(
            command_report(Command::Brightness(percent)),
            Err(PacketError::InvalidBrightness { percent })
        );
    }
}

#[test]
fn jpeg_boundary_lengths_frame_exact_bytes_and_zero_padding() {
    for (length, count) in [(4, 3), (1023, 3), (1024, 3), (1025, 4), (65535, 66)] {
        let jpeg = marker_image(length);
        for key in [0, 5] {
            let reports = frame_jpeg(key, &jpeg).unwrap();
            assert_eq!(reports.len(), count, "length {length}, key {key}");

            let mut start = [0; REPORT_LEN];
            start[1..4].copy_from_slice(b"CRT");
            start[6..9].copy_from_slice(b"BAT");
            start[11..13].copy_from_slice(&(length as u16).to_be_bytes());
            start[13] = key + 1;
            assert_eq!(reports[0], start);
            assert_eq!(
                *reports.last().unwrap(),
                command_report(Command::Flush).unwrap()
            );

            let mut cursor = 0;
            for report in &reports[1..reports.len() - 1] {
                assert_eq!(report[0], 0);
                let chunk_len = (length - cursor).min(REPORT_PAYLOAD_LEN);
                assert_eq!(&report[1..1 + chunk_len], &jpeg[cursor..cursor + chunk_len]);
                assert!(report[1 + chunk_len..].iter().all(|byte| *byte == 0));
                cursor += chunk_len;
            }
            assert_eq!(cursor, length);
        }
    }
}

#[test]
fn published_four_byte_image_vector_is_exact() {
    let reports = frame_jpeg(0, &[0xff, 0xd8, 0xff, 0xd9]).unwrap();
    assert_eq!(
        &reports[0][..14],
        &[
            0x00, 0x43, 0x52, 0x54, 0, 0, 0x42, 0x41, 0x54, 0, 0, 0, 4, 1
        ]
    );
    assert!(reports[0][14..].iter().all(|byte| *byte == 0));
    assert_eq!(&reports[1][..5], &[0, 0xff, 0xd8, 0xff, 0xd9]);
    assert!(reports[1][5..].iter().all(|byte| *byte == 0));
    assert_eq!(
        &reports[2][..9],
        &[0, 0x43, 0x52, 0x54, 0, 0, 0x53, 0x54, 0x50]
    );
    assert!(reports[2][9..].iter().all(|byte| *byte == 0));
}

#[test]
fn jpeg_rejects_bad_keys_lengths_and_markers_without_parsing_the_interior() {
    for key in [6, 8, 255] {
        assert_eq!(frame_jpeg(key, &[]), Err(PacketError::InvalidKey { key }));
    }
    for length in [0, 1, 2, 3, 65536] {
        assert_eq!(
            frame_jpeg(0, &marker_image(length)),
            Err(PacketError::InvalidJpegLength { actual: length })
        );
    }
    for bad in [
        vec![0, 0xd8, 0xff, 0xd9],
        vec![0xff, 0xd8, 0, 0xd9],
        vec![0xff, 0xd8, 0xff, 0],
    ] {
        assert_eq!(frame_jpeg(0, &bad), Err(PacketError::InvalidJpegMarkers));
    }
    assert!(frame_jpeg(3, &[0xff, 0xd8, 0x12, 0x34, 0xff, 0xd9]).is_ok());
}
