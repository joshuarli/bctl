import sys

with open(sys.argv[1], "rb") as image:
    if image.readline().strip() != b"P6":
        raise ValueError("expected PPM P6")
    width, height = map(int, image.readline().split())
    if image.readline().strip() != b"255":
        raise ValueError("expected 8-bit RGB")
    pixels = image.read()
if len(pixels) != width * height * 3:
    raise ValueError("incorrect image size")
magenta = sum(1 for i in range(0, len(pixels), 3)
              if pixels[i] > 180 and pixels[i + 1] < 120 and pixels[i + 2] > 180)
if magenta < width * height * 0.0001:
    raise ValueError("terminal marker missing")
print(f"VM_SCREENSHOT_OK {width}x{height} magenta_pixels={magenta}")
