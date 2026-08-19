#!/usr/bin/env python3
"""Fetch an open-licensed eval slice for the WP-V0.0 bench and emit a schema-exact manifest.

Why this exists
---------------
`fixtures/voice/README.md` is emphatic that the hard slices (proper-noun, code-identifier,
accented-en, whispered) must be recorded by a human, and that stays true — nothing here
substitutes for `bench record`. What this covers is the *other* gap: until now there was no
audio in this repo at all, so there was no way to tell whether a model swap, a decoding-param
change, or a stitcher fix made transcription better or worse. This produces a general-English
regression tripwire that runs in about a minute.

Read the numbers it produces with two caveats, both structural:

1. **Contamination.** Whisper was trained on ~680k hours of scraped web audio, and LibriSpeech
   is one of the most widely mirrored public corpora in existence. It is very likely *in* that
   training data. A good score here means "not broken" — it does not predict accuracy on a
   given user's microphone.
2. **Domain mismatch.** LibriSpeech is read audiobook prose. Dictation is bursty, first-person,
   full of self-corrections, jargon and names. This measures a different thing than the product
   does, which is exactly why it cannot replace the recorded corpus.

Licensing: LibriSpeech is CC-BY 4.0 (Panayotov et al., 2015; openslr.org/12). The audio is
downloaded at bench time into a gitignored directory and never committed, so this repo consumes
the corpus rather than redistributing it.

Usage:
    python3 fixtures/voice/fetch-open-corpus.py [--count N] [--keep-archive]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys
import tarfile
import urllib.request
import wave

HERE = pathlib.Path(__file__).resolve().parent
AUDIO_DIR = HERE / "audio-open"
MANIFEST = HERE / "manifest.open.json"

URL = "https://www.openslr.org/resources/12/test-clean.tar.gz"
# Pinned from the copy this script was written against. The archive is served over plain HTTP(S)
# from a single academic mirror with no signature, so the digest is the only integrity guarantee
# there is — same reasoning as the pinned model weights in voice-asr-whisper's model.rs.
SHA256 = "39fde525e59672dc6d1551919b1478f724438a95aa55f874b576be21967e6c23"

# Reference style vs. ours. LibriSpeech references spell numbers out ("two", "nineteen"); our
# normalizer deliberately emits digits ("2", "19"), and wer.ts does not reconcile the two. Every
# such clip would score a substitution that is a formatting disagreement, not a recognition
# error, so they are excluded rather than silently inflating WER. Measured on ref-3min.wav this
# accounted for 3 of 22 total errors — small, but it is noise pointed in one direction.
NUMBER_WORDS = {
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen",
    "nineteen", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    "hundred", "thousand", "million", "billion",
}

MIN_SECONDS, MAX_SECONDS = 3.0, 15.0


def download(dest: pathlib.Path) -> None:
    if dest.exists() and sha256_of(dest) == SHA256:
        print(f"archive already present and verified: {dest}")
        return
    print(f"downloading {URL} (~331 MB) …")
    urllib.request.urlretrieve(URL, dest)
    actual = sha256_of(dest)
    if actual != SHA256:
        # Refuse rather than warn: a mismatched corpus silently changes every number this
        # harness will ever print, and those numbers are meant to be comparable across runs.
        dest.unlink(missing_ok=True)
        sys.exit(f"archive digest mismatch\n  expected {SHA256}\n  actual   {actual}")
    print("digest verified")


def sha256_of(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def eligible_utterances(tar: tarfile.TarFile) -> list[tuple[str, str]]:
    """(utterance_id, reference_text) for every clip whose reference we can score fairly."""
    out = []
    for member in tar.getmembers():
        if not member.name.endswith(".trans.txt"):
            continue
        fh = tar.extractfile(member)
        if fh is None:
            continue
        for line in fh.read().decode("utf-8").splitlines():
            uid, _, text = line.partition(" ")
            words = text.lower().split()
            if not uid or not words:
                continue
            if any(w in NUMBER_WORDS for w in words):
                continue
            out.append((uid, text))
    out.sort()
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--count", type=int, default=60)
    ap.add_argument("--keep-archive", action="store_true")
    ap.add_argument("--archive", type=pathlib.Path, default=HERE / "test-clean.tar.gz")
    args = ap.parse_args()

    download(args.archive)
    AUDIO_DIR.mkdir(parents=True, exist_ok=True)

    with tarfile.open(args.archive) as tar:
        candidates = eligible_utterances(tar)
        print(f"{len(candidates)} eligible utterances in test-clean")

        # Deterministic, speaker-spread selection: stride through the sorted list rather than
        # taking the first N, which would draw every clip from one or two speakers. No RNG, so
        # re-running picks the identical set and corpus_version stays meaningful.
        stride = max(1, len(candidates) // args.count)
        picked = candidates[::stride][: args.count]

        members = {m.name.rsplit("/", 1)[-1]: m for m in tar.getmembers() if m.name.endswith(".flac")}
        clips = []
        for uid, text in picked:
            member = members.get(f"{uid}.flac")
            if member is None:
                continue
            src = tar.extractfile(member)
            if src is None:
                continue
            flac = AUDIO_DIR / f"{uid}.flac"
            flac.write_bytes(src.read())
            wav = AUDIO_DIR / f"ls-{uid.lower()}.wav"
            # 16 kHz mono PCM s16 — matches LocalAsr::feed_pcm (SPEC §3.3) with no resample step
            # in the bench loop, so the bench measures the engine and not our resampler.
            subprocess.run(
                ["ffmpeg", "-y", "-loglevel", "error", "-i", str(flac),
                 "-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le", str(wav)],
                check=True,
            )
            flac.unlink()
            with wave.open(str(wav)) as w:
                duration = w.getnframes() / float(w.getframerate())
            if not (MIN_SECONDS <= duration <= MAX_SECONDS):
                wav.unlink()
                continue
            clips.append({
                "id": f"ls-{uid.lower()}",
                "audio_path": f"audio-open/{wav.name}",
                "reference_text": text,
                "language": "en-US",
                # Deliberately empty: LibriSpeech is clean read American English and belongs to
                # none of the SPEC §7 hard slices. Tagging it otherwise would let a clean-speech
                # score stand in for a slice it says nothing about.
                "hard_slice_tags": [],
                "bias_terms": [],
                "duration_s": round(duration, 2),
                "placeholder": False,
            })

    MANIFEST.write_text(json.dumps({
        "corpus_version": f"librispeech-test-clean-{len(clips)}clips-v1",
        "notes": (
            "Open-licensed general-English regression slice, fetched by fetch-open-corpus.py "
            "from LibriSpeech test-clean (CC-BY 4.0, openslr.org/12). NOT a substitute for the "
            "recorded hard-slice corpus: this is clean read audiobook prose, it carries no "
            "hard_slice_tags, and LibriSpeech is very likely inside Whisper's training data, so "
            "a good score here means 'not broken' rather than 'accurate for a user'. Clips whose "
            "reference spells out numbers are excluded — our normalizer emits digits and wer.ts "
            "does not reconcile the two, so those score a formatting disagreement as an error."
        ),
        "clips": clips,
    }, indent=2) + "\n")

    if not args.keep_archive:
        args.archive.unlink(missing_ok=True)

    total = sum(c["duration_s"] for c in clips)
    print(f"wrote {MANIFEST} — {len(clips)} clips, {total/60:.1f} min of audio")
    print(f"score it:  textify-voice bench score --manifest {MANIFEST.relative_to(HERE.parent.parent)}")


if __name__ == "__main__":
    main()
