---
title: Voice
description: Paseo voice architecture, local-first model execution, and provider configuration.
nav: Voice
order: 41
category: Configuration
---

# Voice

The daemon synthesizes the speech your agents send with the `speak` tool, and the native clients play it. The native Flutter clients do not capture microphone audio, so composer dictation and voice-mode conversations are not available in them. The dictation and voice-mode settings below configure the daemon for clients that send audio over the protocol.

## Philosophy

Voice is local-first on the **daemon host**. The Rust daemon can run speech locally, or use OpenAI for speech features. Flutter plays audio; mobile clients connect to a remote daemon and do not execute the daemon's speech models on the phone. Voice reasoning/orchestration reuses agent providers installed and authenticated on the daemon host.

This keeps credentials and execution in your environment and avoids introducing a separate cloud-only voice stack.

## Architecture

- Speech I/O: STT and TTS providers per feature (`local` or `openai`)
- Local speech runtime: native Sherpa/ONNX CPU execution for VAD, Parakeet STT, and Kokoro TTS; no Node speech runtime
- Voice LLM orchestration: hidden agent session using your configured provider (`claude`, `codex`, or `opencode`)
- Tooling path: MCP stdio bridge for voice tools and agent control

## Local Speech

Local speech defaults to model IDs `parakeet-tdt-0.6b-v2-int8` (STT) and `kokoro-en-v0_19` (TTS, speaker 0 / voice 00).

Model weights are downloaded on demand into `$PASEO_HOME/models/local-speech` and are not bundled with the application. Keep the generated native speech libraries alongside the daemon runtime.

Native VAD, Kokoro TTS, and Parakeet STT were exercised on Linux; this is not evidence of execution on every target platform. See [testing evidence](https://github.com/getpaseo/paseo/blob/main/docs/testing.md).

The TTS-enabled Sherpa 1.12.28 prebuilts statically include GPL espeak-ng. Generated notices do not resolve corresponding-source/offer, combined-work, MPL/LGPL, CRT, or Rust toolchain obligations. See the [release requirements](https://github.com/getpaseo/paseo/blob/main/docs/release.md). The migration remains unpublished; local TTS is retained and the source license is unchanged.

### Local STT models and language support

| Model ID                    | Languages                                                                                                                                                                                                                                                                    |
| --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `parakeet-tdt-0.6b-v2-int8` | English only (default). Includes punctuation and capitalization.                                                                                                                                                                                                             |
| `parakeet-tdt-0.6b-v3-int8` | 25 European languages, auto-detected: Bulgarian, Croatian, Czech, Danish, Dutch, English, Estonian, Finnish, French, German, Greek, Hungarian, Italian, Latvian, Lithuanian, Maltese, Polish, Portuguese, Romanian, Russian, Slovak, Slovenian, Spanish, Swedish, Ukrainian. |

**To use a non-English language, switch the local STT model to `parakeet-tdt-0.6b-v3-int8`.** v3 detects the spoken language automatically — there is no per-language setting for it. The `language` field below does **not** steer the local Parakeet model (v2 is English-only, v3 auto-detects); it only applies to the OpenAI STT provider.

```json
{
  "version": 1,
  "features": {
    "dictation": {
      "stt": { "provider": "local", "model": "parakeet-tdt-0.6b-v2-int8", "language": "en" }
    },
    "voiceMode": {
      "llm": { "provider": "claude", "model": "haiku" },
      "stt": { "provider": "local", "model": "parakeet-tdt-0.6b-v2-int8", "language": "en" },
      "tts": { "provider": "local", "model": "kokoro-en-v0_19", "speakerId": 0 }
    }
  },
  "providers": {
    "local": {
      "modelsDir": "~/.paseo/models/local-speech"
    }
  }
}
```

For multilingual local dictation, set the model to v3 — it auto-detects the language, so no `language` field is needed:

```json
{
  "version": 1,
  "features": {
    "dictation": {
      "stt": { "provider": "local", "model": "parakeet-tdt-0.6b-v3-int8" }
    }
  }
}
```

The `language` field applies only to the OpenAI STT provider: set `features.dictation.stt.language` for dictation and `features.voiceMode.stt.language` for voice mode. If voice language is omitted, Paseo uses the dictation language before falling back to `en`. It has no effect on the local Parakeet models.

## OpenAI Voice Option

You can switch dictation, voice STT, and voice TTS to OpenAI by setting provider fields to `openai` and providing OpenAI credentials.

```json
{
  "version": 1,
  "features": {
    "dictation": { "stt": { "provider": "openai" } },
    "voiceMode": {
      "stt": { "provider": "openai" },
      "tts": { "provider": "openai" }
    }
  },
  "providers": {
    "openai": {
      "stt": {
        "apiKey": "...",
        "baseUrl": "https://api.openai.com/v1"
      },
      "tts": {
        "apiKey": "...",
        "baseUrl": "https://api.openai.com/v1"
      }
    }
  }
}
```

`providers.openai.stt` covers dictation and voice mode speech-to-text, and `providers.openai.tts` covers voice mode text-to-speech. Because they resolve independently, you can point STT and TTS at different endpoints. Each falls back to `providers.openai.apiKey`/`baseUrl`, then `OPENAI_API_KEY`/`OPENAI_BASE_URL`, when unset. These settings configure only Paseo OpenAI speech traffic, without changing Codex or other OpenAI-backed tools.

Paseo uses these paths under the configured OpenAI base URL:

- dictation STT: `/v1/audio/transcriptions`
- voice mode STT: `/v1/audio/transcriptions`
- voice mode TTS: `/v1/audio/speech`

## Environment Variables

- `PASEO_VOICE_LLM_PROVIDER`, voice agent provider override
- `PASEO_DICTATION_STT_PROVIDER`, `PASEO_VOICE_STT_PROVIDER`, `PASEO_VOICE_TTS_PROVIDER`, speech provider selection (`local` or `openai`)
- `OPENAI_STT_API_KEY`, `OPENAI_STT_BASE_URL`, OpenAI speech-to-text endpoint (dictation + voice mode STT)
- `OPENAI_TTS_API_KEY`, `OPENAI_TTS_BASE_URL`, OpenAI text-to-speech endpoint (voice mode TTS)
- `PASEO_LOCAL_MODELS_DIR`, local model storage directory
- `PASEO_DICTATION_LOCAL_STT_MODEL`, local dictation STT model ID
- `PASEO_VOICE_LOCAL_STT_MODEL`, `PASEO_VOICE_LOCAL_TTS_MODEL`, local voice STT/TTS model IDs
- `PASEO_DICTATION_LANGUAGE`, dictation STT language (OpenAI STT only; ignored by local Parakeet)
- `PASEO_VOICE_LANGUAGE`, voice mode STT language; falls back to `PASEO_DICTATION_LANGUAGE` when unset (OpenAI STT only; ignored by local Parakeet)
- `PASEO_VOICE_LOCAL_TTS_SPEAKER_ID`, `PASEO_VOICE_LOCAL_TTS_SPEED`, optional local voice TTS tuning

## Operational Notes

Voice mode can launch and control agents. Treat voice prompts with the same care as direct agent instructions, especially when specifying working directories or destructive operations.
