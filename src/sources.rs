//! Built-in ROM sets that Search indexes automatically (archive.org, unrestricted at the
//! time of writing; checked 2026-11). Sets that become restricted are skipped at runtime.

/// Every built-in set: the fixed list plus numbered multi-part uploads.
pub fn default_sets() -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = FIXED_SETS.iter().map(|(i, s)| (i.to_string(), *s)).collect();
    for (fmt, sys, parts) in MULTI_PART_SETS {
        out.extend((1..=*parts).map(|n| (fmt.replace("{n}", &n.to_string()), *sys)));
    }
    out.extend(XBOX360_REDUMP.iter().map(|s| (format!("microsoft_xbox360_{s}"), "xbox360")));
    out
}

/// Redump Xbox 360 split by first letter (`microsoft_xbox360_<suffix>`, ~30 TB). These
/// are restricted: they only show up after `romgetter archive-login`.
const XBOX360_REDUMP: &[&str] = &[
    "numberssymbols",
    "a_part1",
    "a_part2",
    "b_part1",
    "b_part2",
    "c_part1",
    "c_part2",
    "d_part1",
    "d_part2",
    "d_part3",
    "e",
    "f_part1",
    "f_part2",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m_part1",
    "m_part2",
    "n_part1",
    "n_part2",
    "o",
    "p",
    "q",
    "r",
    "r_part1",
    "s_part1",
    "s_part2",
    "t_part1",
    "t_part2",
    "u",
    "v",
    "w",
    "x_part1",
    "x_part2",
    "y",
    "z",
];

/// (identifier with `{n}` for the part number, system, number of parts)
const MULTI_PART_SETS: &[(&str, &str, u32)] = &[
    ("PS3-Redump-2024-Individual-Games-Part{n}", "ps3", 34),
    ("Wii-Redump-2024-Individual-Games-Part{n}", "wii", 6),
    // full Redump PS2 set, all regions (parts 1 and 8 restricted as of 2026-11)
    ("PS2-Redump-2024-Individual-Games-Part{n}", "ps2", 17),
    ("ef_Sony_PlayStation1_Redump_Collection_{n}of4", "psx", 4),
    ("ef_sony_psp_part_{n}of3", "psp", 3),
    // plain ISOs, titles # through C
    ("CentralArquivista-XBOX360-part{n}", "xbox360", 4),
];

/// (archive.org identifier, RetroDECK system)
const FIXED_SETS: &[(&str, &str)] = &[
    // cartridge / handheld (No-Intro)
    ("ef_nintendo_entertainment_-system_-no-intro_2024-04-23", "nes"),
    ("ef_nintendo_snes_no-intro_2024-04-20", "snes"),
    ("ef_nintendo_64_no-intro_2024-02-10", "n64"),
    ("ef_Nintendo_Gameboy_No-Intro_2024-04-23", "gb"),
    ("ef_GBC_No-Intro", "gbc"),
    ("ef_gba_no-intro_2024-02-21", "gba"),
    ("ni-n-ds-dec_202401", "nds"),
    ("ni-n-vb", "virtualboy"),
    ("nointro.ms-mkiii", "mastersystem"),
    ("nointro.md", "megadrive"),
    ("nointro.gg", "gamegear"),
    ("nointro.32x", "sega32x"),
    ("nointro.tg-16", "pcengine"),
    ("nointro.atari-2600", "atari2600"),
    ("nointro.atari-5200", "atari5200"),
    ("nointro.atari-7800", "atari7800"),
    ("nointro.ws", "wonderswan"),
    ("nointro.wsc", "wonderswancolor"),
    ("nointro.c64", "c64"),
    ("nointro.ca", "amiga"),
    // disc (Redump)
    ("Redump_PSX_2021_06_04_0-9", "psx"),
    ("Redump_PSX_2021_06_04_A_C", "psx"),
    ("katalog-retro-mibi13-psx-redump", "psx"),
    ("ps2-redump-usa-chd-part-0", "ps2"),
    ("ps2-redump-usa-chd-part-B", "ps2"),
    ("ps2-redump-usa-chd-part-C", "ps2"),
    ("ps2-redump-usa-chd-part-E", "ps2"),
    ("ps2-redump-usa-chd-part-F", "ps2"),
    ("ps2-redump-usa-chd-part-g_202207", "ps2"),
    ("ps2-redump-usa-chd-part-H", "ps2"),
    ("ps2-redump-usa-chd-part-I", "ps2"),
    ("ps2-redump-usa-chd-part-J", "ps2"),
    ("ps2-redump-usa-chd-part-K", "ps2"),
    ("ps2-redump-usa-chd-part-L", "ps2"),
    ("ps2-redump-usa-chd-part-P", "ps2"),
    ("ps2-redump-usa-chd-part-Q", "ps2"),
    ("ps2-redump-usa-chd-part-X", "ps2"),
    ("ps2-redump-usa-chd-part-Z", "ps2"),
    ("sony-playstation-3-rom-collection-main-set", "ps3"),
    ("sony-playstation-3-rom-collection-main-set-part02", "ps3"),
    ("game-cube_202603", "gc"),
    // Xbox 360: Minerva's TorBox cache only holds part of its Xbox 360 torrent
    ("redump-microsoft-xbox-360-games-myrient-unique-files", "xbox360"),
    ("xbox360-games", "xbox360"),
    ("GameCube-Redump-2024-Individual-Games-Part2", "gc"),
    ("3DS-Redump-2024-Individual-Games-Part1", "n3ds"),
    ("3DS-Redump-2024-Individual-Games-Part2", "n3ds"),
    // Switch: no complete set survives on archive.org; these are small multi-game dumps.
    // Most Switch games come from live search or a pasted link.
    ("switch-dumps-nsp-xci", "switch"),
    ("switch-dump-nsp-xci", "switch"),
    ("backup-data", "switch"),
    ("sega-saturn-redump", "saturn"),
    ("sega-mega-cd-sega-cd-redump", "segacd"),
    ("sega-dreamcast-redump", "dreamcast"),
    // arcade: MAME 0.260 non-merged (every zip self-contained; names are MAME short names)
    ("mame-0.260-roms-non-merged", "mame"),
    ("nec-pc-engine-cd-turbografx-cd-redump", "pcenginecd"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::SYSTEMS;

    #[test]
    fn default_sets_use_known_systems() {
        let sets = default_sets();
        for (item, sys) in &sets {
            assert!(SYSTEMS.iter().any(|(f, _)| f == sys), "{item} → {sys}");
        }
        assert!(sets.iter().any(|(i, _)| i == "PS3-Redump-2024-Individual-Games-Part34"));
        let mut ids: Vec<_> = sets.iter().map(|(i, _)| i).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), sets.len(), "duplicate set ids");
    }
}
