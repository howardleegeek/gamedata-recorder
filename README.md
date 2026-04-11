<div align="center">

# GameData Recorder

### **Play Games. Record Screen. Get Paid.**

Record your gameplay automatically and earn money while you play.
Your data helps AI companies build the next generation of world models.

[![Build](https://github.com/howardleegeek/gamedata-recorder/actions/workflows/build.yml/badge.svg)](https://github.com/howardleegeek/gamedata-recorder/actions/workflows/build.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Website](https://img.shields.io/badge/Website-Live-blue)](https://gamedata-recorder.vercel.app)

</div>

---

**🌐 官网**: https://gamedata-recorder.vercel.app

## How It Works

1. **Download & Install** — One-click installer, no configuration needed
2. **Play Games** — Just play normally. Recording starts automatically when a game is detected
3. **Get Paid** — Your gameplay data is uploaded automatically. Earnings show up in your dashboard

That's it. No buttons to press, no settings to configure.

## What Gets Recorded

| Data | Format | Purpose |
|------|--------|---------|
| Game video | H.265/HEVC, 1080p, 30fps | Visual training data |
| Input logs | JSON Lines (keyboard, mouse, controller) | Action labels |
| Metadata | JSON (game title, system specs, FPS stats) | Data catalog |

## Privacy & Safety

- Only records while a game is in the foreground
- Stops automatically when you alt-tab or exit the game
- Does NOT record desktop, browsers, or non-game apps
- Does NOT use any injection or hooks that trigger anti-cheat
- PII (faces, usernames in notifications) is automatically blurred server-side
- All recordings are uploaded over encrypted connections (TLS 1.3)
- MIT open-source — you can audit every line of code

## System Requirements

- Windows 10/11
- GPU with hardware encoding (NVIDIA, AMD, or Intel)
- 2 GB free disk space
- Internet connection for uploads (WiFi recommended)

## Earning Potential

| Hours/Month | Estimated Earnings |
|-------------|-------------------|
| 20 hrs | ~$10 |
| 40 hrs | ~$20 |
| 80 hrs | ~$40+ |

Earnings vary based on data quality, game diversity, and market demand.
Premium data (engine metadata from supported games) earns 2-4x more.

## Building from Source

```bash
# Requires Windows + Rust toolchain + OBS SDK
git clone https://github.com/howardleegeek/gamedata-recorder.git
cd gamedata-recorder
cargo build --release
```

## Credits

Based on [OWL Control](https://github.com/Overworldai/owl-control) by Overworld AI (MIT License).

## License

MIT
