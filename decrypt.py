from Crypto.Cipher import ChaCha20_Poly1305
from iced_x86 import *
import re
from PIL import Image
from io import BytesIO

image = Image.new("RGB", (1900, 900), "black")

with open("harlem_shake", "rb") as f:
    executable = f.read()

# update offsets when re-building
key = executable[0x19DCC6:0x19DCE6]
code_offset = 0x95609
code_end = 0x95D43

def decrypt(img_off, img_len, x, y):
    nonce = executable[img_off:img_off + 12]
    img = executable[img_off + 12: img_off + img_len]

    cipher = ChaCha20_Poly1305.new(key=key, nonce=nonce)
    char_img = Image.open(BytesIO(cipher.decrypt(img)))

    image.paste(char_img, (x, y))

img_off = 0
img_len = 0
img_pos_x = 0
img_pos_y = 0

for instr in Decoder(64, executable[code_offset:code_end], ip=0):
    # yolo regex matching just works
    if m := re.match(r"lea rcx,\[(?P<off>\w+)h\]", str(instr)):
        img_off = int(m.group("off"), 16) + code_offset
    if m := re.match(r"mov r8d,(?P<len>\w+)h", str(instr)):
        img_len = int(m.group("len"), 16)
    if m := re.match(r"mov esi,(?P<x>\w+)h", str(instr)):
        img_pos_x = int(m.group("x"), 16)
    if m := re.match(r"mov edx,(?P<y>\w+)h", str(instr)):
        img_pos_y = int(m.group("y"), 16)
    if m := re.match(r"call 0FFFFFFFFFFFFECD7h", str(instr)):
        decrypt(img_off, img_len, img_pos_x, img_pos_y)

    if instr.ip > (code_end - code_offset):
        break

image.save("flag.png")
