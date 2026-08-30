# Equilibrium

A desktop application for structured self-help work, following protocols with researched efficacy. Everything is stored encrypted on your own machine, the model runs locally, and nothing leaves the computer.

[Русский](README.ru.md)

## What this is

The program runs an exercise step by step: it reviews what happened with the action you planned, helps you see the recurring chain — what came before, what you felt, what you did instead, what followed — and takes the next action down to specifics: when, where, how long, what could get in the way.

What it is not: therapy, treatment, diagnosis, or a substitute for a professional. The program does not assess your state and does not diagnose, because doing that over text is not possible, and imitating it does harm.

Not intended for: acute thoughts of self-harm, mania, psychosis. Those need a person.

## How it works

**The code drives the steps, not the language model.** The model writes the sentence for the current step and decides nothing: it does not choose the action for you, does not move to the next topic, and cannot skip a step by being persuasive. A state machine holds the sequence.

**Every message passes a risk check before generation.** The check is deterministic and does not consult the model: large models behave inconsistently on exactly the middle-band phrasings like "sometimes I think I don't want to live". When it fires, the exercise stops and crisis resources are shown — as static text, with no model involved.

**The model's reply is reviewed before it reaches you.** Agreement, unsolicited advice, ready-made option lists in place of your own words, performed feelings — all rejected. Failed once: regenerate with the objection attached. Failed twice: the step asks its own plain question. A plain honest question beats a fluent violation of the protocol.

**Change is measured in your words, not in questionnaires.** You name two to four difficulties in your own wording and rate them zero to ten. Idiographic measures are more sensitive to change than normative scales: compared directly, PSYCHLOPS against CORE-OM gives an effect size of 1.53 against 1.06.

**Data lives in a single encrypted file.** The database is held in memory; what reaches the disk is an encrypted blob (XChaCha20-Poly1305, key derived from your passphrase with Argon2id). An unencrypted database file never exists at any moment. The passphrase is not stored anywhere: lose it and the records are gone.

The safety log records the time, the kind of trigger and what was done. It contains no message text.

## What is deliberately absent

- Streaks, badges, points, any gamification. It does not improve retention, and in lawsuits against chatbot developers this kind of design is described as a design defect.
- Notifications with emotional pressure. A reminder arrives only for what you scheduled yourself, at the time you chose.
- A name, an avatar or a biography for the program.
- Free-form talk that never reaches an action. Venting on its own does not work: across 154 studies the effect size is −0.02.
- Personality typologies. MBTI, the Enneagram and the like have no predictive validity, and their test-retest unreliability at the type level makes tracking change meaningless.
- Automatic emotion detection from text or voice.

## Install

Download the build for your system from the releases page and run it.

A local model through [Ollama](https://ollama.com) is needed for dialogue:

```
ollama pull qwen3:8b
```

Without Ollama the program still runs: each step asks its own plain question, but there is no live dialogue.

## Building from source

Rust and Node.js 20+ are required.

```
npm install
npm run tauri build
```

There are no system dependencies: the cryptography and SQLite are built from source, and OpenSSL is not required.

## Limitations

An honest list of what is wrong here.

**No clinical trial has been run.** The protocol draws on behavioural activation research, but this application itself has not been tested. The one trial of a generative therapeutic agent (Therabot, NEJM AI, 2025) ran under continuous staff supervision: over eight weeks with 106 people it took 28 interventions, 13 of them to correct the bot's own replies. Autonomous operation has been demonstrated by no one.

**Crisis lines are listed only for the US and the UK.** Numbers for other countries are absent, because an unverified crisis line is worse than none. If you know a verified number with an official source, send it in.

**Local model quality is limited.** Models that fit in 8 GB of video memory follow instructions only some of the time. The reply review compensates, at the cost of replacing some replies with the step's plain question.

## Licence

AGPL-3.0.
