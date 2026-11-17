`import-tone.mp3` is a synthetic 440 Hz, 0.25-second mono tone, generated locally for the native audio import test. It contains no recorded speech.

Generation (ffmpeg is only a fixture-generation tool, not an app dependency):

```sh
ffmpeg -nostdin -hide_banner -loglevel error -f lavfi -i sine=frequency=440:duration=0.25 -ac 1 -ar 44100 -b:a 32k -map_metadata -1 import-tone.mp3
```
