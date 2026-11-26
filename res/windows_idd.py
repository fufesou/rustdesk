import hashlib
import io
import shutil
import urllib.request
import zipfile
from pathlib import Path


RELEASE_URL = ('https://github.com/fufesou/RustDeskIddDriver/releases/download/'
               'build_github_ci_test_sign')
ARCHIVE_SHA256 = {
    'x64': '8ac9c8d2b859a424afd5d394a5281b1e8059f28fcf3aaaccfb50e37f372d3484',
    'arm64': '46910e612238289cbe6b14fab4656deb68b0b5c80a45fc7d250415afef231b33',
}


def stage_rustdesk_idd(arch, output_dir):
    expected = ARCHIVE_SHA256[arch]
    filename = f'RustDeskIddDriver_{arch}.zip'
    with urllib.request.urlopen(f'{RELEASE_URL}/{filename}') as response:
        data = response.read()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f'SHA-256 mismatch for {filename}')

    driver_dir = Path(output_dir) / 'RustDeskIddDriver'
    driver_dir.mkdir(exist_ok=True)
    members = ('RustDeskIddDriver/RustDeskIddDriver.inf',
               'RustDeskIddDriver/RustDeskIddDriver.dll',
               'RustDeskIddDriver/rustdeskidddriver.cat',
               'RustDeskIddDriver.cer', f'vc_redist.{arch}.exe')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for member in members:
            with archive.open(member) as source, (driver_dir / Path(member).name).open('wb') as dest:
                shutil.copyfileobj(source, dest)
