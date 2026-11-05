`icon.svg` is the source. Regenerate the rest after editing it:

```sh
nix run nixpkgs#librsvg -- -w 1024 -h 1024 icon.svg -o icon.png
mkdir AppIcon.iconset
for s in 16 32 128 256 512; do
  sips -z $s $s icon.png --out AppIcon.iconset/icon_${s}x${s}.png
  sips -z $((s*2)) $((s*2)) icon.png --out AppIcon.iconset/icon_${s}x${s}@2x.png
done
iconutil -c icns AppIcon.iconset -o AppIcon.icns && rm -r AppIcon.iconset
```
