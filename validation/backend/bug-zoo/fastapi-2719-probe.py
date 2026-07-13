import json
import sys
from pathlib import Path

from fastapi import FastAPI
from pydantic import BaseModel
from starlette.testclient import TestClient


class RespModel(BaseModel):
    example_field: str


app = FastAPI()


@app.get("/", response_model=RespModel)
def get_resp():
    return None


out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
client = TestClient(app)
schema_response = client.get("/openapi.json")
schema_response.raise_for_status()
(out / "openapi.json").write_text(json.dumps(schema_response.json(), sort_keys=True))

try:
    response = client.get("/")
except Exception as error:
    (out / "outcome.json").write_text(
        json.dumps(
            {"kind": "rejected", "error_type": type(error).__name__, "message": str(error)},
            sort_keys=True,
        )
    )
    print(f"rejected: {type(error).__name__}: {error}")
else:
    (out / "response.json").write_text(json.dumps(response.json(), sort_keys=True))
    (out / "outcome.json").write_text(
        json.dumps({"kind": "response", "status": response.status_code}, sort_keys=True)
    )
    print(response.status_code, response.json())
