import json
import os
import sys

from fastapi import Depends, FastAPI
from pydantic import BaseModel
from starlette.testclient import TestClient

app = FastAPI()


class ModelB(BaseModel):
    username: str


class ModelC(ModelB):
    password: str


class ModelA(BaseModel):
    name: str
    description: str = None
    model_b: ModelB


async def get_model_c() -> ModelC:
    return ModelC(username="test-user", password="test-password")


@app.get("/model", response_model=ModelA)
async def get_model_a(model_c=Depends(get_model_c)):
    return {
        "name": "model-a-name",
        "description": "model-a-desc",
        "model_b": model_c,
    }


client = TestClient(app)
output = sys.argv[1]
os.makedirs(output, exist_ok=True)
response = client.get("/model")
assert response.status_code == 200
schema = client.get("/openapi.json")
assert schema.status_code == 200
with open(os.path.join(output, "response.json"), "w", encoding="utf-8") as stream:
    json.dump(response.json(), stream, sort_keys=True)
with open(os.path.join(output, "openapi.json"), "w", encoding="utf-8") as stream:
    json.dump(schema.json(), stream, sort_keys=True)
print(json.dumps(response.json(), sort_keys=True))
