"""Regenerate anonymous recognition ROIs from the existing historical captures.

Usage: python generate.py D:/Games/Mahjong/design
Requires Pillow; never connects to a running game. Only the detector's fixed
regions are retained; names, scores and avatars are excluded.
"""
from pathlib import Path
import sys
from PIL import Image

source = Path(sys.argv[1])
out = Path(__file__).parent
regions = [(13.7, 7.95, 15.35, 8.65), (11.25, 7.95, 13.1, 8.50),
           (5.4, 6.3, 7.5, 6.85), (8.3, 6.3, 10.6, 6.85),
           (6.7, 1.6, 10.0, 2.55), (1.5, 2.63, 14.6, 2.87),
           (1.5, 6.0, 14.6, 6.24)]
captures = [
    ('continue', 'game-end-screens/1-standings.jpg', (0, 0, 2560, 1440)),
    ('rank', 'game-end-screens/2-rank.jpg', (0, 0, 2560, 1440)),
    ('rematch', 'game-end-screens/3-chest-rematch.jpg', (0, 0, 2560, 1440)),
    ('ask', 'game-end-screens/4-rematch-confirm.jpg', None),
    ('reward-1', 'game-end-screens/5-reward-user.webp', (14, 34, 1407, 818)),
    ('reward-2', 'game-end-screens/5-reward-user-2.webp', (0, 37, 1381, 817)),
    ('hand-result', 'panel-shots/003031-result-hule-8s.png', None),
    ('table', 'panel-shots/003047-table.png', None),
]
for name, filename, box in captures:
    image = Image.open(source / filename).convert('RGB')
    # Full-view captures have a 16:9 game canvas, with no window chrome.
    if box is not None:
        image = image.crop(box)
    print(name, image.size)
    image = image.resize((960, 540), Image.Resampling.LANCZOS)
    clean = Image.new('RGB', image.size)
    # Keep only controls on normal screens; the other detector regions can
    # overlap names in standings. Reward captures retain their title/rules.
    keep = regions if name.startswith('reward-') else regions[:4] if name == 'ask' else regions[:2]
    for left, top, right, bottom in keep:
        roi = tuple(round(v * 60) for v in (left, top, right, bottom))
        clean.paste(image.crop(roi), roi[:2])
    clean.save(out / (name + '.png'))
