#!/usr/bin/env python3
"""Inspect real Wasm module resource declarations without executing it.

This is a binary metadata reader, not another Wasm engine or a validator.
"""

import argparse
import hashlib
import json
from pathlib import Path


class Reader:
    def __init__(self, data):
        self.data = memoryview(data)
        self.offset = 0

    def bytes(self, count):
        end = self.offset + count
        if end > len(self.data):
            raise ValueError("truncated Wasm section")
        result = self.data[self.offset:end]
        self.offset = end
        return result

    def uleb(self):
        result = 0
        for shift in range(0, 70, 7):
            byte = self.bytes(1)[0]
            result |= (byte & 127) << shift
            if byte < 128:
                return result
        raise ValueError("oversized Wasm integer")

    def name(self):
        return bytes(self.bytes(self.uleb())).decode("utf-8")

    def limits(self):
        flags = self.uleb()
        result = {"flags": flags, "minimum": self.uleb()}
        if flags & 1:
            result["maximum"] = self.uleb()
        return result


def profile(path):
    data = Path(path).read_bytes()
    module = Reader(data)
    if bytes(module.bytes(8)) != b"\0asm\1\0\0\0":
        raise ValueError("expected a Wasm 1 module")
    report = {"module": str(path), "sha256": hashlib.sha256(data).hexdigest(),
              "bytes": len(data), "imports": [], "memories": [], "tables": []}
    while module.offset < len(data):
        identifier = module.bytes(1)[0]
        section = Reader(module.bytes(module.uleb()))
        match identifier:
            case 1:
                count = section.uleb()
                report["types"] = count
                report["max_function_parameters"] = 0
                report["max_function_results"] = 0
                for _ in range(count):
                    if section.bytes(1)[0] != 0x60:
                        raise ValueError("non-function Wasm type")
                    parameters = section.uleb()
                    section.bytes(parameters)
                    results = section.uleb()
                    section.bytes(results)
                    report["max_function_parameters"] = max(report["max_function_parameters"], parameters)
                    report["max_function_results"] = max(report["max_function_results"], results)
            case 2:
                for _ in range(section.uleb()):
                    item = {"module": section.name(), "name": section.name()}
                    kind = section.bytes(1)[0]
                    item["kind"] = kind
                    match kind:
                        case 0:
                            item["type"] = section.uleb()
                        case 1:
                            item["element_type"] = section.bytes(1)[0]
                            item["limits"] = section.limits()
                        case 2:
                            item["limits"] = section.limits()
                        case 3:
                            item["global_type"] = list(section.bytes(2))
                        case 4:
                            item["attribute"] = section.bytes(1)[0]
                            item["type"] = section.uleb()
                        case _:
                            raise ValueError("unknown import kind")
                    report["imports"].append(item)
            case 3:
                report["defined_functions"] = section.uleb()
            case 4:
                for _ in range(section.uleb()):
                    element_type = section.bytes(1)[0]
                    report["tables"].append({"element_type": element_type, **section.limits()})
            case 5:
                for _ in range(section.uleb()):
                    report["memories"].append(section.limits())
            case 6:
                report["globals"] = section.uleb()
            case 9:
                report["element_segments"] = section.uleb()
            case 10:
                report["largest_function_body_bytes"] = 0
                report["max_declared_locals"] = 0
                for _ in range(section.uleb()):
                    size = section.uleb()
                    body = Reader(section.bytes(size))
                    locals_count = 0
                    for _ in range(body.uleb()):
                        locals_count += body.uleb()
                        body.bytes(1)
                    report["largest_function_body_bytes"] = max(report["largest_function_body_bytes"], size)
                    report["max_declared_locals"] = max(report["max_declared_locals"], locals_count)
            case 11:
                report["data_segments"] = section.uleb()
    report["non_wasi_imports"] = [item for item in report["imports"]
                                  if item["module"] != "wasi_snapshot_preview1"]
    report["inspection_scope"] = "declarations only; not module validation or execution"
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("module", type=Path)
    args = parser.parse_args()
    print(json.dumps(profile(args.module), indent=2))
