from pathlib import Path
import struct


ICONS = Path(__file__).resolve().parents[1] / "apps/ternilo-desktop/icons"


def png(name, size):
    data = (ICONS / name).read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack(">II", data[16:24]) != (size, size):
        raise ValueError(f"{name} must be a {size}x{size} PNG")
    return data


def main():
    images = [(32, png("32x32.png", 32)), (128, png("128x128.png", 128)), (256, png("128x128@2x.png", 256))]
    offset = 6 + 16 * len(images)
    directory = []
    for size, image in images:
        directory.append(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(image), offset))
        offset += len(image)
    icon = struct.pack("<HHH", 0, 1, len(images)) + b"".join(directory) + b"".join(image for _, image in images)
    (ICONS / "icon.ico").write_bytes(icon)
    chunks = []
    for kind, image in [(b"ic07", images[1][1]), (b"ic08", images[2][1]), (b"ic09", png("icon.png", 512))]:
        chunks.append(kind + struct.pack(">I", len(image) + 8) + image)
    payload = b"".join(chunks)
    (ICONS / "icon.icns").write_bytes(b"icns" + struct.pack(">I", len(payload) + 8) + payload)
    print("Generated ICO and ICNS from the existing desktop PNG assets")


if __name__ == "__main__":
    main()
