# VHS tape — records the burn-after-reading demo GIF.
# Run from the repo root: vhs demo/burn.tape
Output docs/src/assets/burn.gif
Set FontSize 13
Set Width 880
Set Height 300
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms
Set Padding 16

Hide
Type `export PATH="$PWD/rs/target/release:$PATH"`
Enter
Type "clear"
Enter
Show

Type "python3 demo/burn_once.py"
Sleep 600ms
Enter
Sleep 5s
