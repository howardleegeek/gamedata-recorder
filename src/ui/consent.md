# GameData Labs — Gameplay Data Collection

GameData Labs Inc.

Contact: howard.linra@gmail.com

## Purpose of Study

You are invited to participate in a research study aiming to collect combined game and control data for the purposes of training world models and subsequently AI agents. The curated dataset will be open sourced and made publicly available for research purposes. This software will record your game and inputs to potentially contribute to this dataset. There is no minimum required participation.

## Procedures — What Exactly We Record

**Please read this section carefully. It describes the actual technical scope
of the recording, not a sugar-coated summary.**

If you agree to participate, then **while any whitelisted game is running**,
our software will:

- **Record your entire primary monitor** — the full-screen video frame,
  including **any window that overlays the game** (Discord popups, browser
  windows, chat clients, notifications, taskbar, and anything else visible
  on that display). The recording is of the display surface, not the game
  window. If you drag another app on top of the game, it will appear in the
  recording.
- **Log all keyboard and mouse events globally** — not scoped to the game
  window. The capture is installed at the Windows Raw Input layer with
  `RIDEV_INPUTSINK`, which delivers every key press, mouse click, mouse
  movement, and scroll event anywhere on your system to this software for
  as long as the hook is installed. This includes input you type into other
  applications while a game is running (passwords you type into a password
  manager, text in a browser, messages in a chat app, etc.).
- **Log gamepad button and axis events** via XInput polling.
- Store this data locally for research purposes.

## Data Collection and Privacy

- The recording software starts the hook and display capture when a
  whitelisted game is launched and stops them when the game closes or when
  no input activity is detected for a sufficient period.
- The software cannot record microphone audio.
- The software records game audio only; it does not record arbitrary desktop
  audio streams.
- Data is stored locally on your machine and only uploaded when you manually
  press the Upload button.
- Further processing and cleaning will be done before any open-source release.
  During this process the data is stored securely and anonymized.
- Upon full open-source release there will be no identifying information in
  the dataset.
- We do not attempt to filter out overlay windows or other on-screen content
  that is not the game. If you do not want something recorded, do not have
  it visible on your primary monitor while a whitelisted game is running.

## Potential Risks

- Using GameData Recorder in multiplayer games may result in account bans, as anti-cheat systems may flag it as suspicious software
- We strongly recommend using GameData Recorder only in single-player games to avoid potential issues

## Voluntary Participation

Your participation is entirely voluntary. You may:

- Choose not to participate
- Stop recording at any time
- Request deletion of your recorded data
- Withdraw from the study without penalty

## Compensation

- There is no compensation for this study

## Questions or Concerns

For questions, contact howard.linra@gmail.com

## Content Policy & Legal Terms

**Important:** You agree not to upload any content that is:

- Illegal in your jurisdiction or country of origin
- Malicious, harmful, or inappropriate
- In violation of any applicable laws or regulations

If you upload content that violates these terms, we will take necessary and proportional actions, which may include:

- Removal of your content
- Suspension or termination of your access
- Reporting to appropriate authorities
- Other legal remedies as required by law

## Consent

By clicking "Accept" below you confirm that:

- You have read and understood the above information
- You are 18 years or older
- You voluntarily agree to participate
- You understand you can withdraw at any time
- You agree to comply with the content policy and legal terms
