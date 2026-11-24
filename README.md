# romgetter

A TUI for finding ROMs, staging them on TorBox, and downloading them on demand into a RetroDECK `roms/<system>/` folder.

```
cargo run --release
```

## Steam Deck

Build a static x86-64 Linux binary on your Mac. It has no dependencies on the Deck.

```
brew install zig && cargo install --locked cargo-zigbuild && rustup target add x86_64-unknown-linux-musl   # once
./scripts/build-steamdeck.sh        # → dist/romgetter-steamdeck
```

Copy it over (USB stick, or `scp dist/romgetter-steamdeck deck@steamdeck:~/romgetter` if you've enabled SSH on the Deck). Then, in Desktop Mode, open Konsole:

```
chmod +x ~/romgetter
~/romgetter
```

On the Deck the config goes in `~/.config/romgetter/config.toml`, and ROMs go to `~/retrodeck/roms` by default. Enter the API keys and, if RetroDECK lives on the SD card, the roms folder (`/run/media/<card>/retrodeck/roms`) on the **5 Config** tab; Steam's keyboard (Steam+X) types them.

### Game Mode with a controller

`scripts/steamdeck-launch.sh` (copied to `dist/romgetter-launch.sh` by the build) opens romgetter full-screen in Konsole with a large font, so it can run as a non-Steam game.

1. Copy `romgetter-launch.sh` next to the binary (`~/romgetter-launch.sh`) and `chmod +x` it.
2. In Desktop Mode, go to Steam → **Add a Game → Add a Non-Steam Game → Browse** and pick `romgetter-launch.sh`. Rename the shortcut to "romgetter" in its Properties if you like.

romgetter reads the controller itself (Steam's virtual pad, `/dev/input`), so leave the shortcut's controller layout on Steam's default **Gamepad** layout. No keyboard mapping is needed.

| Button | Does |
|---|---|
| D-pad / left stick | move (hold to scroll); ← → collapse and expand systems in Library |
| A | the item's default action: add (Search); download, or open/close a system (Library) |
| B | the item's menu of actions (download directly, change system, remove, delete from disk, top 20) · in a menu or text box: back |
| X | search (Search tab) or filter (Library), and open Steam's on-screen keyboard |
| Y / Menu | tab menu: download all ready, paste a link, sort, collapse/expand all, rescan, quit |
| LB / RB | switch tab |
| LT / RT | scroll a page |

In a game's menu, the **System** row changes the system with ← →. In a system's header menu, **Add the top 20 games** adds Metacritic's best for that system. If Steam's keyboard doesn't appear, press **Steam + X**.

Keyboard users get the same menus: Space on an item, `m` for the tab menu. The single-letter shortcuts still work.

To change the font size, set `ROMGETTER_FONT_SIZE` in the shortcut's launch options, e.g. `ROMGETTER_FONT_SIZE=18 %command%`.

## Setup

Everything below can be set on the **5 Config** tab, which saves this file. On first run a config is created at:

- Linux / Steam Deck: `~/.config/romgetter/config.toml`
- macOS: `~/Library/Application Support/romgetter/config.toml`
- override with `ROMGETTER_CONFIG=/path/to/config.toml`

```toml
torbox_api_key = "..."            # or export TORBOX_API_KEY
roms_dir = "/home/deck/retrodeck/roms"
poll_secs = 5
real_debrid_api_key = "..."       # optional; or export REAL_DEBRID_API_KEY
browser = "flatpak run org.mozilla.firefox"   # optional; default: Firefox/Chrome, else the system's
downloads_dir = "/home/deck/Downloads"        # optional; where the browser saves files

[sources]                          # written by the Sources tab: sources switched away from their default
vimm = true                        # Vimm's Lair, off unless switched on
"minerva:Redump/Sony - PlayStation 2" = false

[[extra_sets]]                     # optional: more archive.org sets to search
item = "some-archive-org-identifier"
system = "snes"                    # "" = detect
```

Your library is stored as `library.json` next to the config on macOS, or in `~/.local/share/romgetter/` on Linux. Override with `ROMGETTER_LIBRARY`.

`roms_dir` defaults to `~/retrodeck/roms`. If RetroDECK lives on an SD card, point it at the `roms` folder there.

### archive.org login (restricted sets)

Some archive.org sets are "restricted": only a logged-in account can download their files (e.g. the Redump Xbox 360 set `microsoft_xbox360_a_part1` … `_z`). Log in once on the Config tab (archive.org → email, then password), or from a shell:

```sh
romgetter archive-login you@example.com   # asks for the password (not echoed)
romgetter archive-logout                  # forget the session
```

Or set `ARCHIVE_ORG_EMAIL` / `ARCHIVE_ORG_PASSWORD`. Only the session cookies are saved (`[archive_login]` in the config), not the password. While logged in, restricted sets show up in Search, and their games download directly from archive.org (`○ direct`), because TorBox can't log in for you.

## Tabs

**1 Search**: type to find games. Results come from:

- **Minerva** (minerva-archive.org, the torrent-based successor to Myrient): every No-Intro and Redump system folder for systems RetroDECK runs, about 140 folders and roughly 130,000 games. Each folder is one torrent. Adding a Minerva game puts that folder's torrent on TorBox and later downloads just the one file; games from the same folder share one TorBox torrent. **Only some folders are usable through TorBox**: romgetter asks TorBox which files of each folder it has cached (daily) and hides the rest from search, so an archive.org copy of the same game shows instead. Folders up to 25 GB are fine uncached (TorBox downloads the whole folder). The big disc systems (PS1/PS2/PS3/PSP, Xbox/Xbox 360, Wii/Wii U, GameCube, Dreamcast, 3DS: 4 to 42 TB each) are cached on TorBox for one game each as of 2026-11, so their games come from archive.org. Listings are indexed in the background on first run (under a minute) and refreshed weekly. When the same game is in several places and TorBox has Minerva's copy, Minerva wins.
- **ROM sets already in your TorBox account**: add a whole-set torrent (e.g. the "1G1R - Redump - Sony - PlayStation" magnets on the Emulation General wiki's ROM & ISO sites page) to TorBox yourself, and once it's cached romgetter finds it at startup and indexes the games inside. Any cached torrent whose folder name says the system counts (Minerva folders are left to Minerva; PC repacks are skipped). TorBox packs a torrent with 100+ files into one zip: romgetter reads the zip's directory over HTTP (no download) and pulls a game out of it like a zipped Minerva folder. Smaller torrents keep their files loose (at least 3 games needed), and TorBox's file list is the index. A loose torrent built around one big zip (500 MB+, e.g. `NES_Test_Collection_3/NES.zip`) is indexed from inside that zip, and games come out of it the same way (stored or deflated entries). A torrent whose name says ROM set but no system (`REDUMP`, `No-Intro`, `TOSEC`, `1G1R`, `ROMs`) is indexed as a set of several systems, each game taking the system its folder names (`REDUMP/Sony - PlayStation/…`); a folder that names none (`REDUMP/GAMES/`) takes the system from the set's catalog file (`ReDump PS2 USA.dat`), else its games are left out. When TorBox serves only the start of a zip but that part has its own directory, the games in it are indexed and downloadable; the rest show the set as cut off. DOS collections (`eXoDOS…`, `MS-DOS`, `Total DOS Collection`) go to `roms/dos/`, where DOSBox-Pure runs each game's zip as is. romgetter reads the whole TorBox list, past TorBox's 1000-per-page limit. These sets pack each game as a `.7z`, which is unpacked into `roms/<system>/` after download (MAME `.7z` romsets stay packed). Torrents removed from TorBox drop out of search at the next start. Set `use_torbox_zips = false` to leave them out.
- **98 built-in archive.org sets** (No-Intro for cartridge and handheld systems, Redump for disc systems including PS3, PS2, PSP, PS1, GameCube, Wii and 3DS; see `src/sources.rs`). They're indexed in the background on first run (a few minutes), cached, and refreshed every 14 days. Results appear instantly as you type.
- **live archive.org search**, run 0.7 s after you stop typing (or press Enter), for single-game uploads that aren't in any set. Phone, PC and modern-console uploads are filtered out.
- **MAME 0.260 (non-merged)** for the `mame` system. MAME names its zips by short name (`sf2.zip`), so romgetter loads libretro's MAME list (short name → title, refreshed monthly) and search and the Library show and match full titles (`street fighter arcade`). Files keep their short names, which MAME needs. Games that need a CHD (hard disk or CD images, e.g. CPS3's Street Fighter III, which is only ~130 KB here) won't run from the zip alone.
- **Vimm's Lair** (vimm.net/vault), a last resort, so **off by default**: turn it on in the Sources tab (Live search → Vimm's Lair, `Space`). It's searched live alongside archive.org; its results say "vimm.net (browser download)". Add "vimm" to a search to see only these. Vimm puts every download behind a human check, so romgetter can't fetch them. Enter on a Vimm game opens its page in a browser (Firefox or Chrome if installed, so it works from Game Mode; set `browser` to choose). Press Download there, and romgetter watches the downloads folder (`downloads_dir`, default `~/Downloads`). When a file matching the game's title finishes, romgetter moves it into `roms/<system>/`, unpacking a `.7z` (and dropping Vimm's `.txt` notice).
- **The IF Archive** (ifarchive.org): free interactive fiction for RetroDECK's Gargoyle, about 2,300 story files. romgetter reads the archive's `Master-Index.xml` (indexed in the background, refreshed every 14 days) and keeps only files Gargoyle opens: Z-machine (`.z1`–`.z8`, `.zblorb`) go to `roms/zmachine/`; Glulx (`.ulx`, `.gblorb`, `.blb`), TADS (`.gam`, `.t3`), ADRIFT (`.taf`), Alan, AGT, Level 9, Magnetic Scrolls and JACL go to `roms/if/`. Zipped releases, web builds, source code and superseded versions (`old/`) are skipped. Each game shows the title and author from its description ("Photopia, by Adam Cadre  [photopia.z5]"), so you can search by author (`emily short`). `infocom`, `zcode` and `zmachine` filter to Z-machine games; `interactive fiction` or `text adventure` filter to the `if` system (a bare "if" stays an ordinary search word). These are free files, so they **never go through TorBox**: adding one downloads it straight away, and the Library shows `○ free` until it's on disk. Set `use_ifarchive = false` to leave them out. The classic Infocom games are still sold commercially and aren't on the archive.
- any `extra_sets` from the config.

A query matches the file name (region and version tags included), the set or upload title (e.g. "redump", "no-intro"), and the system:

- `god of war ps3`, `zelda game boy advance`, `sonic genesis`, `crash ps1`: system names and aliases act as filters
- `final fantasy sony`, `metroid nintendo`: manufacturer words match the system's label
- `ps3` alone: browse a system
- words match at the start of a word, so `sonic` doesn't find "Supersonic"

Search is the tab romgetter opens on. Results are grouped under system headers (in Library order, foldable like the Library: `Enter` on a header or `←`/`→`), then sorted alphabetically by name. Each game shows once, with "N sources": every region, revision, file type and source of it shares the row (`Crash Bandicoot (USA).7z`, `Crash Bandicoot (Europe) (EDC).zip`, `crash_bandicoot_(usa).ZIP`, a scene `0006 - Polarium (U)(Trashman).zip` next to No-Intro's `Polarium (USA, Australia).zip`, axekin's `The Legend of Zelda- Twilight Princess (axekin.com).rvz` next to Redump's `Legend of Zelda, The - Twilight Princess (USA).zip`). Case, spaces vs underscores, punctuation, articles, site and release-group tags, scene release numbers and a tag naming the game's own system don't count. What makes a different product does: disc/side numbers, DLC/updates/themes, Virtual Console and other re-releases, translations, saves (see `src/names.rs`). The source list is ordered: `⚡` cached on TorBox right now (a torrent in your account, or a Minerva file TorBox has) first, then within that and the rest: the finished game over betas/demos/bad dumps, USA over World over Europe over the rest, the latest revision, then the indexed sets, live archive.org, Vimm. `✓` = in library, `●` = on disk.
Keys: `Enter` adds a game with one source, or lists its sources to pick from · `a` adds the best source straight away · `Space` actions · `S` change the target system.

From any tab, `s` jumps to this Search box; `/` searches (filters) within the current tab.

**2 Library**: your games, grouped by system. **Every game in the library is sent to TorBox automatically**: when added, on startup if it isn't there yet, and again if TorBox drops it. Failures are retried every 5 minutes.
Keys: `Enter` download into RetroDECK (or fold/open a system header) · `D` download every `☁ ready` game (2 at a time; the rest show `⏳ queued`) · `t` add the system's top 20 games · `Space` `←` `→` `c` `e` folding · `d` direct download (skip TorBox) · `p` paste a magnet/URL · `/` filter · `S` system · `o` sort by name or by size (biggest systems and games first; each system header shows its total size) · `x` remove · `X` delete from disk · `r` rescan + refresh

**Grouping.** Each system is a group you can fold: `Enter`/`Space` on a header toggles it, `←`/`→` folds or opens the system you're in, and `c`/`e` fold or open all of them. Libraries with more than 40 games open with every system folded. An open system shows at most 20 games, the ones needing attention first (downloading, ready on TorBox, in progress, errors, not sent), then what's already on disk. Past that there's a `… N more` row; Enter on it filters the list to that system. While a filter is typed, every match shows, with no folding and no limit.

**Roms folder sync.** On startup (and on `r`), romgetter looks through `roms_dir/<system>/` and adds anything it finds that the library doesn't already know about, so you can see what you already have. These entries show `● on disk · found in roms folder`, are never sent to TorBox, and disappear from the library if you delete the files. Search marks matching results `●` (same system and file name) and won't re-add them. `romgetter scan` does the same sync from the command line.

Status badges:

- `… sending`: being added to TorBox
- `☁ 42%`: TorBox is fetching it
- `☁ ready`: cached on TorBox; Enter downloads it
- `⇣ 63%`: downloading to your roms folder
- `● on disk`: present in `roms_dir/<system>/`
- `✗ torbox`: TorBox rejected it (auto-retried)

"On disk" is checked against the actual files, so deleting a ROM by hand shows up.

For sites without an API (most of the FMHY list), copy the magnet or download link and paste it in the Library with `p`.

**3 Textures**: HD texture packs for the GameCube and N64 games on disk. Each game that has packs is listed with them underneath, each with a line on what it changes and its style. Selecting a pack shows its full write-up below the list: whether it's complete, its region, and the source's overview of what it retextures.
Keys: `Enter` installs a pack and turns it on, or turns an installed one on/off · `Space` (B) lists every build (DDS/PNG, Native/Lite, Rice/GlideN64 cache, mirrors) plus the pack's page · `m` → refresh the lists · `r` refresh.

- **Where packs come from**: [EmulationKing](https://emulationking.com/nintendo/gamecube/texturepacks/) (GameCube and N64), [rapka's list](https://github.com/rapka/dolphin-textures/blob/master/PACKS.md) and [putachi's free GameCube list](https://putachi.net/ultimate-gamecube-hd-texture-pack-list/). They're fetched when the tab is first opened (about 100 pages), cached, and refreshed weekly. The same pack on several lists (same game, same author) shows once with all of its links.
- **Matching** uses the disc ID for GameCube (read from the `.iso`/`.rvz` header; Dolphin names texture folders by it) and the ROM's internal name for N64 (what GLideN64 names them by). It falls back to the title.
- **Downloads**: file-host links (MEGA, Google Drive, MediaFire, Dropbox) go through TorBox first, then Real-Debrid. EmulationKing's own files download directly, because neither service accepts its server. Links to Patreon, Gumroad, GBAtemp or forum posts open in a browser (`🌐 web page only`).
- **Installing** unpacks the zip/7z/rar (rar uses SteamOS's `7z`/`unrar`/`bsdtar`) and puts the texture folder where RetroDECK's emulators look: `texture_packs/Dolphin/Textures/<disc ID>/` and `texture_packs/retroarch-core/Mupen64Plus/hires_texture/<ROM name>/` (GLideN64 `.hts` caches go to `…/Mupen64Plus/cache/`). Turning a pack off moves its folder to `texture_packs/romgetter/`, so only one pack per game is ever on. romgetter never replaces a folder you put there yourself. Installing an N64 pack also turns on `mupen64plus-txHiresEnable` in RetroArch's core options. Dolphin's "Load Custom Textures" is on by default in RetroDECK. A pack takes effect the next time the game starts.

`romgetter textures` prints which packs fit the games on disk; `romgetter textures install <words>` installs the first matching pack (e.g. `romgetter textures install conker`).

**4 Sources**: every set Search uses, grouped by where it comes from: Minerva No-Intro, Minerva Redump, your TorBox torrents, the built-in archive.org sets, restricted archive.org sets, your `extra_sets`, and the IF Archive, plus the sites searched live as you type (archive.org search, Vimm's Lair). Each group header shows how many sets and games it holds, and how many are off; groups start closed. Each set shows its status (`⚡ cached` on TorBox, `● indexed`, `… indexing`, `○ waiting`, `✗ failed` with the reason, `🔒 login` for restricted sets until you log in to archive.org on the Config tab), its system, game count, size, and when it was indexed.
Any source can be switched off and on: `Space` on a set or site toggles it, and on a group header it turns the whole group off (or, if it's all off, back on). Off sets show `○ off`, drop out of Search and of the Library's source matching, and aren't indexed; switching one back on indexes it if needed. The choice is saved in the config's `[sources]` table and applies at once, with no restart; everything starts on except Vimm's Lair. Old switches carry over: `use_vimm = true` turns Vimm's Lair on, and `use_minerva`, `use_default_sets`, `use_torbox_zips` or `use_ifarchive = false` turn that group's sets off here.
Keys: `Space` on/off · `Enter` opens a group, or on a set lists its actions: turn it off/on, search that system (jumps to Search with the system typed in), or index the set again now, or **cache it to TorBox / Real-Debrid**. That last one is offered for torrent-backed sets (Minerva folders) under 1 TB, once per configured service, unless TorBox already has the whole torrent (in your account or fully cached). It sends the whole torrent (every file) to that service so its games download straight away later; the set's row shows `⇣ TorBox downloading 40%` while it caches and `✓ cached on TorBox` when done (TorBox progress also shows after a restart; Real-Debrid's only for this session) · `←`/`→` close/open the group you're in · `m` opens or closes every group.

**5 Config**: the settings in the config file, changed in place and saved straight away.

- **Accounts**: the TorBox and Real-Debrid API keys (typed hidden, shown as `••••••••abcd`). Each key is checked with its service when the tab opens and after it's changed: `✓ you@example.com · pro`, `✓ user · premium until 2026-12-31`, or `✗` with the reason. A key set in the environment (`TORBOX_API_KEY`, `REAL_DEBRID_API_KEY`) wins over the config, and the row says so. **archive.org** logs in (email, then password; only the session is kept) or out.
- **Folders**: the roms folder (`✗ not there` if it's missing) and the browser's downloads folder watched for Vimm games. `~` stands for the home folder.
- **Other**: the browser command for Vimm pages, and how often TorBox is checked while games download.
- **Your extra sets**: archive.org sets searched on top of the built-in ones. `+ Add a set…` takes an identifier or an archive.org URL and indexes it right away; `Enter` on a set removes it.

The downloads folder, browser, check interval and extra sets apply at once. Keys, the archive.org login and the roms folder apply at startup: the tab then shows `Restart romgetter` at the top (and `5 Config (restart)` in the tab bar), which starts romgetter again in place with the same arguments.
Keys: `Enter` changes the setting (or switches it), `Space` lists its actions (change, back to the default / remove the key / log out, check the key), `Esc` cancels typing. On a controller: A changes, B lists actions, X is off here.

## System detection

Strongest signal first:

1. The file's own extension, when it's unambiguous (`.sfc` → snes, `.gba` → gba, `.nsp` → switch).
2. Folder names in the file's path (`Nintendo - Game Boy Advance/...`).
3. The set or item's system: from its title, then identifier, then a vote over its file extensions, then its subject tags, then a peek inside a few zips (for sets), then its description. Platforms RetroDECK doesn't emulate (PS4/PS5, Xbox One/Series, iOS, Android, J2ME, ...) never match a nearby system.
4. For pasted links: the URL's file name or the magnet's `dn=`, then each file TorBox returns.

If nothing matches, the game shows `?`. Pick a system with `S` (or the System line in the `Space` menu, ←/→); nothing is saved to a guessed folder.

## Notes / limits

- If your TorBox account has "allow zipped" on (the default), TorBox packs torrents with 100+ files into one big zip. That includes every Minerva folder. romgetter then reads the zip's directory with HTTP range requests and copies out just your game, so it never downloads the whole set.
- A Minerva game TorBox can't deliver (its file isn't cached and the folder is over 25 GB) shows `✗ torbox not cached on TorBox …` in the Library and is re-checked every 6 hours. TorBox can't download one file of an uncached torrent, and these folders are terabytes, so look for the game from another source in Search. A pasted magnet for a whole Minerva folder is refused for the same reason.
- TorBox sometimes lists a packed Minerva zip at the folder's full size but serves only its first part (seen: 2.5 GB of the 898 GB Saturn zip), so there's no zip directory to read. romgetter checks the served size before reading. When a copy is cut off, it stops using TorBox for that folder for a week: its games move to Real-Debrid if you have a key, and otherwise search shows the archive.org copy.
- Real-Debrid allows 250 API requests a minute. romgetter spaces its requests (about 3 a second), backs off on `429`, polls all games with one listing request, and sets up one game at a time.
- **Real-Debrid** (optional, `real_debrid_api_key`): Minerva games TorBox can't deliver go to Real-Debrid instead, which can download one file out of a huge folder torrent. Each game gets its own RD torrent with only that file selected; romgetter reuses one already on your RD account. With a Real-Debrid key, search shows every Minerva game again. RD downloads uncached files from Minerva's peers, so speed depends on who is seeding that folder (0 seeders means it waits). The badge shows `RD downloading · N seeders`, then `☁ ready … on Real-Debrid`; Enter downloads it, then PS3/Xbox 360 unzip and the PS3 disc key work as usual.
- PS3 ISOs that are already decrypted (common in non-Redump uploads, e.g. `Name [BLUS30943].iso`) need no key; romgetter checks `EBOOT.BIN` and skips the key step. For an encrypted ISO without its Redump name, the key is matched by title and the serial's region, and only when exactly one key fits.
- PS3 Redump dumps are encrypted ISOs, zipped. After a PS3 download romgetter unzips the ISO (deleting the zip) and fetches its disc key from Minerva's Redump "Disc Keys" torrents via TorBox, saving `roms/ps3/<Game>.iso` + `<Game>.dkey`. Current RPCS3 loads the encrypted ISO directly when the key sits next to it. If the key step fails, the ISO stays and `Enter` retries just the key. Unzipping briefly needs room for both the zip and the ISO.
- archive.org items flagged `access-restricted-item` need a logged-in account and are skipped.
- TorBox web downloads are rate-limited (about 60 per hour per token).
- Removing a game (`x`) leaves its local files and the TorBox copy alone. `X` deletes a game's files from the roms folder: the game, its tracks, a PS3 `.dkey`, and any leftover `.part`. Press it twice to confirm; any other key cancels. A downloaded game stays in the library so Enter can download it again, and a game found by the roms-folder scan is removed from the library.
- Original Xbox: xemu can't load a Redump image (video partition + game partition) or a zip. romgetter unzips it and keeps only the game partition (from 387 MiB in, as the xemu docs describe), checking for the `MICROSOFT*XBOX*MEDIA` header. A full Redump `.iso` already in `roms/xbox/` shows `◐ convert`; Enter trims it. Xbox 360 zips are unzipped too (Xenia loads the ISO).
- Minerva's Redump "GDI Files" and "SBI Subchannels" folders hold only track descriptors or subchannel data (a few hundred bytes per game, useless alone) and aren't indexed. The Dreamcast games are the ~100 MB+ zips from the main "Sega - Dreamcast" folder (cue/bin tracks, which Flycast loads).
- Disc games are unzipped after download where the emulator can't run them zipped (checked against each core's source):
  - always unzipped: PS2, PSP, GameCube, Wii, Neo Geo CD, CD-i, Jaguar CD and Amiga CD32 (their emulators read no zips); Dreamcast (Flycast treats every zip as a Naomi arcade cartridge, which is why Crazy Taxi failed); Saturn (Beetle Saturn reads zips only for ST-V arcade cartridges); Wii U and Switch (Cemu and the Switch emulators read no zips, and ES-DE doesn't even list a zipped game).
  - unzipped only when the zip holds more than one file: PS1, 3DO, Sega CD, PC Engine CD and PC-FX. RetroArch extracts one file for these cores, so a single-image zip works as-is, but a `.cue` with `.bin` tracks doesn't.
  - Zips already on disk that need it show `◐ zipped`, and Enter unzips them.
- In the Library, a disc's `.cue`/`.gdi`/`.m3u`/`.ccd` and the track files it lists count as one game. The roms-folder scan doesn't add the tracks as separate entries, and it removes any it added before.
- Archives (`.zip`/`.7z`) of cartridge games are kept as-is, since RetroArch cores load them directly.

## Top 20 per system

`t` on a system in the Library (its header or any of its games) adds that system's 20 best-rated games. For N64/PS1 onward (N64, PS1, PS2, PS3, PSP, Vita, GameCube, Wii, Wii U, Switch, Dreamcast, Xbox, Xbox 360, GBA, DS, 3DS) the list is Metacritic's all-time top by Metascore. Metacritic has nothing older, so NES, SNES and Genesis use a built-in list of the usual consensus classics (`src/metacritic.rs`). Each title is matched to the best clean dump in the search index (USA/World first, no betas or demos, only sources that can deliver it). Games already in the library or on disk are skipped, and the status line names any titles with no copy (e.g. download-only games like Braid). `romgetter top <system>` prints the matches without changing anything.

## Headless search

`romgetter search god of war ps3` prints the top results, using the same search code as the TUI (it never touches TorBox or your library).

## Tests and lints

```
./scripts/check.sh                                  # before committing: fmt, clippy (warnings = errors), tests
cargo fmt                                           # fix formatting (rustfmt.toml)
cargo clippy --fix --allow-dirty                    # apply clippy's suggestions
cargo test                                          # offline unit tests
cargo test -- --ignored live                        # live archive.org / Minerva / Vimm / TorBox checks (some need keys)
cargo test detect_report -- --ignored --nocapture   # system-detection coverage over real sets
romgetter bench                                     # time startup, search and drawing on this machine's data (offline)
```

Lints are set in `Cargo.toml` under `[lints]`: clippy's `all` and `pedantic` groups as warnings, with a short list of pedantic lints turned off (each with its reason), plus `unsafe_code = "forbid"`.
