# Supported devices

RustKnob reads the battery of these devices over HID, without the vendor's software. The protocols live in the
family files next to this list, and the app fetches only the ones for devices that are plugged in. Any Bluetooth
device whose battery Windows shows, and any HID device that reports the standard battery usage, also work, with no
file needed. Most entries were not tried on real hardware.

Tested here: Razer Viper V3 Pro and MonsGeek FUN60 Max (Rongyuan receiver). The DPI cards cover Razer Viper V3 Pro,
Logitech PRO X SUPERLIGHT 2, ATK A9 Plus, ASUS ROG Gladius III and Logitech G435 (`cards.json`).

| Family file | Devices named in it | Products |
|---|---|---|
| `8bitdo` | 8BitDo Ultimate 2 Wireless, 8BitDo Ultimate 2, 8BitDo Ultimate 3-mode, 8BitDo Ultimate 2C Wireless, 8BitDo Ultimate 2.4G, 8BitDo Ultimate Bluetooth, 8BitDo Pro 3, 8BitDo Pro 2, 8BitDo SN30 Pro, 8BitDo SF30 Pro | 19 product ids; every product of vendor 2DC8 |
| `airoha` | Audeze Maxwell, Audeze Maxwell 2, ENDORFY Viro Wireless, Turtle Beach Stealth 700X Gen 3, Turtle Beach Stealth 700PC Gen 3, Turtle Beach Stealth 700 Gen 3, AKG N9 Hybrid | 11 product ids |
| `ajazz-ak992` | AJAZZ AK992 | 1 product id |
| `akko` | Akko V9 Dash Pro, Akko Dash V9 Ultra | 2 product ids |
| `alienware` | Alienware AW920H | 1 product id |
| `areson` | Redragon wireless mouse, Fantech Helios Go XD5, Redragon Invader Pro | 5 product ids |
| `astro` | ASTRO A50 X, ASTRO A50 Gen 5, ASTRO A50 Gen 4, ASTRO A50 Gen 3, ASTRO A20 Wireless | 16 product ids |
| `atk-bitmouse` | ATK Zero, ATK 8K Nano mouse | 5 product ids |
| `aula-by` | AULA wireless keyboard, MCHOSE wireless keyboard | 4 product ids |
| `aula-hero` | AULA HERO 68 Air | 1 product id |
| `aula-sc580` | AULA SC580 | 2 product ids |
| `aula-x9` | AULA X9 Ultra / V9 | 1 product id |
| `barracuda-pro` | Razer Barracuda Pro | 1 product id |
| `beitong` | BEITONG KP20, BEITONG KP40D | 2 product ids |
| `cards` | Viper V3 Pro, Superlight 2, ATK A9 Plus, G435, ROG Gladius III | 52 product ids |
| `cherry` | CHERRY MX 3.0S Wireless, Cherry XTRFY wireless mouse | 3 product ids |
| `chimera` | Chimera V2 | 1 product id |
| `coolermaster` | Cooler Master MM731, Cooler Master MM712, Cooler Master wireless mouse | 3 product ids |
| `corsair` | Corsair Sabre V2 Pro Ultralight, Corsair Sabre V2 Pro Wireless MG | 28 product ids; every product of vendor 1B1C |
| `dareu` | DAREU A980 Pro, DAREU A84 | 3 product ids |
| `darkrim` | DARKRIM SHADOW | 1 product id |
| `darmoshark` | Darmoshark M3, Darmoshark M3S, Darmoshark M3S Pro | 3 product ids |
| `drivercore` | Arbiter Studio Akitsu, Arbiter Studio Akitsu 3950, Arbiter Studio Akitsu Medium, LAMZU Maya X, LAMZU Maya X V2, LAMZU Tachi, LAMZU Tachi Lite, LAMZU Inca, LAMZU Maya, LAMZU Paro, LAMZU Thorn, LAMZU Thorn V2, LAMZU Atlantis OG Champion, LAMZU Atlantis Mini, LAMZU Maya M, LAMZU Orcus, LAMZU Mini, LAMZU wireless mouse | 54 product ids; every product of vendor 373E, 37B0 |
| `endgame` | Endgame Gear 4k wireless mouse, Endgame Gear XM2we | 2 product ids |
| `evision` | ATK Z87, Redragon wireless mouse | 4 product ids |
| `eweadn` | EWEADN S9 Ultra | 2 product ids |
| `finalmouse` | Finalmouse UltralightX, Finalmouse Starlight X | 3 product ids |
| `flydigi` | Flydigi Vader 5 Pro, Flydigi Apex 5 | 2 product ids |
| `fractal` | Fractal Design Scape | 2 product ids |
| `gamesir` | GameSir Nova, GameSir Cyclone 2 | 7 product ids |
| `glorious` | Glorious Model O Wireless, Glorious Model D Wireless, Glorious Model O- Wireless, Glorious Model D- Wireless, Glorious Model O 2 Wireless, Glorious Model D 2 PRO, Hitscan Hyperlight X, Hitscan Hyperlight N, G-Wolves HTM Plus, G-Wolves HSK Pro 2.0, G-Wolves HTXU, G-Wolves Fenrir Pro, G-Wolves VUK, G-Wolves HT-S2, G-Wolves Fenrir Max, G-Wolves HTS Ultra, G-Wolves HTR, G-Wolves Fenrir, G-Wolves HT-S2 Pro, G-Wolves HSK Pro, G-Wolves HTX Mini, G-Wolves HTS Plus, G-Wolves HTX, G-Wolves HSK Plus, G-Wolves HSK Lite, G-Wolves HTR Pro, G-Wolves HSK Pro ACE, G-Wolves HTS Plus ACE, G-Wolves HTX ACE, G-Wolves HSK Plus ACE, G-Wolves Lycan, G-Wolves WARG, G-Wolves HTS Plus Pro, G-Wolves Fenrir Asym, G-Wolves HTX Mini Asym, G-Wolves Fenrir Max Asym, G-Wolves VUK Mini, WLMouse YING, Hitscan Hyperlight, G-Wolves wireless mouse | 73 product ids; every product of vendor 258A, 3770, 33E4 |
| `hyperx` | HyperX Cloud III Wireless, HyperX Cloud II Core Wireless, HyperX Cloud III S Wireless, HyperX Cloud II Wireless, HyperX Cloud Flight, HyperX Cloud Flight S, HyperX Cloud Alpha Wireless, HyperX Cloud Alpha 2 Wireless, HyperX Pulsefire Haste 2 Core Wireless, HyperX Pulsefire Haste 2 Wireless, HyperX Pulsefire Haste 2 Mini, HyperX Cloud Stinger Core Wireless, HyperX Cloud Stinger Core Wireless DTS, HyperX Cloud Core Wireless | 28 product ids |
| `jabra` | (names from the device) | every product of vendor 0B0E |
| `jbl` | JBL Quantum 350 Wireless, JBL Quantum 610 Wireless, JBL Quantum 360 Wireless, JBL Quantum 650 Wireless, JBL Quantum 650X Wireless, JBL Quantum 950 Wireless, JBL Quantum 950X Wireless, JBL Quantum 520 Wireless, JBL Quantum 520X Wireless, JBL Quantum 850 Wireless | 17 product ids |
| `keychron` | Keychron Nape Pro, Keychron mouse | 5 product ids; every product of vendor 3434, 3434 |
| `kysona` | Redragon wireless mouse, VXE Dragonfly R1, HECATE mouse, Hitscan Hyperlight | 13 product ids; every product of vendor 3554, 373B, 3770, 33E4, 3710 |
| `logitech-g435` | Logitech G435 | 1 product id |
| `logitech` | Logitech wireless device, Logitech wireless device, Logitech wireless device, Logitech wireless device, Logitech wireless device, Logitech wireless device, Logitech wireless device | 13 product ids; every product of vendor 046D |
| `machenike` | Machenike L7 | 2 product ids |
| `maono` | MAONO PD200W | 1 product id |
| `mchose` | MCHOSE A5 Pro / Pro Max, MCHOSE A5 Pro, MCHOSE A5 Pro Max, MCHOSE G3 V2, MCHOSE K87 V3, MCHOSE V9 Pro 2 | 22 product ids; every product of vendor 5253 |
| `misc-mice` | Motospeed X6, Delux M800 Pro, Ajazz mouse, AJAZZ wireless mouse, Incott mouse | 9 product ids; every product of vendor 248A, 249A, 3151 |
| `moondrop` | MOONDROP GM01 Pro | 1 product id |
| `msi` | MSI VERSA 300 ELITE WIRELESS, MSI Clutch GM31 Lightweight Wireless, MSI Clutch GM41 Lightweight Wireless | 5 product ids |
| `niceboy` | Niceboy MW400 | 1 product id |
| `ninjutso` | Ninjutso Sora V2, Sprime PM1 | 9 product ids |
| `nuphy` | NuPhy WH80 | 1 product id |
| `nzxt` | NZXT Lift Elite | 2 product ids |
| `plantronics` | Poly / Plantronics headset | 2 product ids |
| `playstation` | DualShock 4, DualSense, DualSense Edge | 9 product ids |
| `pressplay` | Press Play ICARUS V2 Nearlink | 1 product id |
| `pulsar` | Pulsar X2 V3, Pulsar X2 V3 eS, Pulsar wireless mouse | 4 product ids |
| `raigor` | (names from the device) | 2 product ids |
| `rapoo` | Rapoo VT0, Rapoo VT0 Max, Rapoo VT1, Rapoo VT1 Max, Rapoo VT2, Rapoo VT2 Max, Rapoo VT3, Rapoo VT3 Max, Rapoo VT3 Max V2, Rapoo VT3S, Rapoo VT3S Max, Rapoo VT7, Rapoo VT7 Max, Rapoo VT9, Rapoo VT9 Max, Rapoo VT3 Pro, Rapoo VT1 Pro Max, Rapoo VT3 Pro Max | 35 product ids; every product of vendor 24AE |
| `razer-headset` | Razer BlackShark V2 HyperSpeed, Razer Barracuda X, Razer BlackShark V2 Pro, Razer BlackShark V3 X, Razer BlackShark V3 Pro, Razer BlackShark V3, Razer BlackShark V3 Pro for Xbox | 13 product ids |
| `razer` | (names from the device) | 16 product ids; every product of vendor 1532 |
| `rog` | ROG Pelta, ROG Delta II, ROG Cetra Open Wireless, ROG Omni Receiver, ROG Azoth, ROG Azoth Extreme, ROG Azoth Extreme SE, ROG Azoth X, ROG Falchion, ROG Falchion RX, ROG Falchion RX Low Profile, ROG Strix Scope II 96, ROG Strix Scope II 96 RX, ROG Strix Scope RX TKL, ROG Chakram, ROG Chakram X, ROG Gladius II Wireless, ROG Gladius III Wireless, ROG Gladius III Aimpoint, ROG Gladius III Aimpoint EVA-02, ROG Harpe Ace Aim Lab Edition, ROG Harpe Ace Extreme, ROG Harpe Ace Mini, ROG Harpe II Ace, ROG Keris Wireless, ROG Keris Wireless EVA Edition, ROG Keris Wireless Aimpoint, ROG Keris II Ace, ROG Keris II Origin, ROG Keris II Origin KJP, ROG Pugio II, ROG Spatha X, ROG Strix Carry, ROG Strix Impact II Wireless, ROG Strix Impact III Wireless | 63 product ids |
| `rongyuan` | MonsGeek keyboard, Attack Shark R3 | 81 product ids |
| `royal-kludge` | RK MG6 Pro | 4 product ids |
| `scyrox` | Scyrox V12M | 1 product id |
| `semico` | Attack Shark X98, AJAZZ AK820 Max Plus | 3 product ids |
| `sinowealth` | Sinowealth wireless mouse | 1 product id |
| `sonix` | EPOMAKER P75, RK R75, Redragon Arlokks K742B, M1 V5 VIA, Lucky65V2, Lofree Flow2, MSI FORGE TKL W FRIEREN, Essential75, NUT65, RK 2.4G Dongle, WEIKAV MAX102, NuPhy Air75V2, SKN Qinglong, Lemokey Link, Wireless Link, Spade65-QMK-2.4GHz | 20 product ids |
| `sony-inzone` | Sony INZONE H9 II, Sony INZONE H5, Sony INZONE Buds | 3 product ids |
| `steam` | Steam Controller | 5 product ids |
| `steelseries-elite` | SteelSeries Arctis Nova Elite | 1 product id |
| `steelseries` | SteelSeries Arctis 1 Wireless, SteelSeries Arctis 7X, SteelSeries Arctis 7, SteelSeries Arctis Pro, SteelSeries Arctis 9, SteelSeries Arctis Pro Wireless, SteelSeries Arctis Nova Pro Wireless, SteelSeries Arctis 7+, SteelSeries Arctis Nova 3P Wireless, SteelSeries Arctis Nova 5, SteelSeries Arctis GameBuds, SteelSeries Arctis Nova 7, SteelSeries Aerox 3 Wireless, SteelSeries Aerox 3 Wireless CS2 Dragon Lore Edition, SteelSeries Aerox 3 Wireless Gen 2, SteelSeries Aerox 5 Wireless, SteelSeries Aerox 9 Wireless, SteelSeries Prime Wireless, SteelSeries Aerox 5 Wireless Diablo IV Edition, SteelSeries Rival 3 Wireless, SteelSeries Rival 3 Wireless Gen 2, SteelSeries Rival 650 Wireless, SteelSeries Rival Pro | 51 product ids |
| `switch-pro` | (names from the device) | 1 product id |
| `vaxee` | VAXEE wireless mouse, VAXEE 8K Wireless, ZYGEN NP-01S V2 Wireless | 3 product ids |
| `vgn-zhou` | (names from the device) | 1 product id |
| `via` | Wireless keyboard, WAVE 75, Aurora 80, PMO P75, PMO 80 | 5 product ids |
| `wlmouse` | WLMouse Sword X 8K, WLMouse Beast X 8K, WLMouse Beast G, WLMouse mouse, WLMouse Beast X Pro, WLMouse Beast X V2, WLMouse Beast X Mini Pro, WLMouse Beast MAX, WLMouse Beast X Mini | 14 product ids |
| `xenta` | Wireless mouse | 5 product ids |
| `zenith` | Zenith Pro | 2 product ids |
| `zowie` | ZOWIE EC-CW, ZOWIE DW mouse, ZOWIE U2 | 4 product ids; every product of vendor 04A5 |

817 entries in 75 family files. The exact product ids are in `index.json`.
