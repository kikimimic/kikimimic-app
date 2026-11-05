# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "mlx-audio @ git+https://github.com/Blaizzy/mlx-audio@03a4d99e6cac5c13523c1027761ab33abfdcd30d",
# ]
# ///
"""Run the MLX port of Nemotron-3-Diarization on a completed recording."""

import sys
from pathlib import Path

from mlx_audio.vad import load


def main(audio: str, output: str) -> None:
    model = load("mlx-community/Nemotron-3-Diarization", strict=True)
    result = model.generate(audio)
    Path(output).write_text(result.text + "\n")


if __name__ == "__main__":
    main(*sys.argv[1:3])
