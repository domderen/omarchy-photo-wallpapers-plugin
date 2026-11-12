import io

from PIL import Image, ImageCms, ImageOps


def oriented_srgb(path, *, formats=None):
    with Image.open(path, formats=formats) as original:
        original.load()
        image = ImageOps.exif_transpose(original)
        profile = image.info.get("icc_profile")
        if profile:
            try:
                return ImageCms.profileToProfile(
                    image,
                    ImageCms.ImageCmsProfile(io.BytesIO(profile)),
                    ImageCms.createProfile("sRGB"),
                    outputMode="RGB",
                )
            except (OSError, ValueError, ImageCms.PyCMSError):
                pass
        return image.convert("RGB")
