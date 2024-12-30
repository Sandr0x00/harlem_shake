from PIL import Image, ImageDraw, ImageFont
import os
import string
from Crypto.Cipher import ChaCha20_Poly1305

charactersList = string.ascii_letters + string.digits + "_{}'"
font = "/usr/share/fonts/TTF/Inconsolata-Regular.ttf"

path = "letters"

if not os.path.exists(path):
    os.mkdir(path)

key = bytes([145, 177, 108, 160, 218, 93, 51, 44, 185, 144, 149, 150, 190, 95, 105, 24, 240, 225, 25, 86, 245, 86, 133, 241, 17, 209, 5, 196, 165, 236, 95, 88])

for index,character in enumerate(charactersList):
    fpath = f"{path}/{character}.png"
    if os.path.exists(fpath):
        os.remove(fpath)
    sz = 150
    fnt = ImageFont.truetype(font, sz)
    img_w, img_h = int(sz * 2/3), sz + 20
    img = Image.new('1', (img_w, img_h), color='black')
    d = ImageDraw.Draw(img)
    # center alignment
    d.text((10, 0), character, font=fnt, fill=255, align="center")
    img.save(fpath)

    print(f'let letter_{character} = include_bytes!("../letters/{character}.png");')


    with open(fpath, "rb") as f:
        raw = f.read()

    cipher = ChaCha20_Poly1305.new(key=key, nonce=os.urandom(12))
    encrypted, tag = cipher.encrypt_and_digest(raw)

    with open(fpath, "wb") as f:
        f.write(cipher.nonce + encrypted + tag)
