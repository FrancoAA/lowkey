"""open-whisper: local, open-source speech-to-text web app."""

import os

# Model downloads use Hugging Face's plain HTTP transfer, not hf_xet. On this project's test
# machine hf_xet stalled mid-file more than once (no error, no progress, forever), which would
# leave a first run on "Downloading…". Plain HTTP resumes a partial file and fails with an
# error we can retry. Must be set before huggingface_hub is imported. A user can still choose
# otherwise by setting the variable.
os.environ.setdefault("HF_HUB_DISABLE_XET", "1")

__version__ = "0.1.0"
