group "default" {
  targets = ["artifacts"]
}

group "artifacts-all" {
  targets = ["artifacts-linux-amd64", "artifacts-linux-arm64"]
}

target "artifacts" {
  dockerfile = "Dockerfile.artifacts"
  context    = "."
  target     = "artifacts"
  output     = ["type=local,dest=./target/artifacts"]
}

target "artifacts-linux-amd64" {
  inherits  = ["artifacts"]
  platforms = ["linux/amd64"]
  output    = ["type=local,dest=./target/artifacts/linux-amd64"]
}

target "artifacts-linux-arm64" {
  inherits  = ["artifacts"]
  platforms = ["linux/arm64"]
  output    = ["type=local,dest=./target/artifacts/linux-arm64"]
}
