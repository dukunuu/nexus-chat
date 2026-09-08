"""List downloaded Hugging Face cache candidates for mlx-lm, without network access."""
import os
from pathlib import Path

home = Path(os.environ.get("HF_HOME", Path.home() / ".cache" / "huggingface"))
hub = Path(os.environ.get("HF_HUB_CACHE", home / "hub"))
for repo in sorted(hub.glob("models--*")):
    snapshots = repo / "snapshots"
    if any(
        (snapshot / "config.json").is_file()
        and any(weight.is_file() for weight in snapshot.glob("*.safetensors"))
        for snapshot in snapshots.glob("*")
    ):
        print(repo.name.removeprefix("models--").replace("--", "/"))
