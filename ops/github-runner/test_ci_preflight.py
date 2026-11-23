"""Docker inspect format and isolation regressions for the hosted preflight."""

from copy import deepcopy
import unittest
from unittest.mock import patch

import ci_preflight as preflight


# Hosted run 45198208490 used Engine 28.0.4 / Compose 2.38.2. The
# Compose implementation places a named volume in Binds (toBindString)
# while anonymous scratch is sent as a typed mount with no source.
PROJECT = "metro-runner-preflight-45198208490-1-51c811fe13aa"
STATE = f"{PROJECT}-state"
SCRATCH = "a" * 64
JOBS = f"{PROJECT}-jobs"
OUTSIDE = f"{PROJECT}-out"


def fixture():
    def volume(name, target):
        return {"Type": "volume", "Name": name, "Destination": target, "Driver": "local", "RW": True,
                "Source": f"/var/lib/docker/volumes/{name}/_data"}

    def container(uid, primary, networks):
        return {
            "Config": {"User": f"{uid}:{uid}", "Env": ["HTTPS_PROXY=http://egress:3128"]},
            "HostConfig": {"ReadonlyRootfs": True, "Privileged": False, "CapDrop": ["ALL"],
                           "SecurityOpt": ["no-new-privileges:true"], "Binds": None, "Mounts": [],
                           "PortBindings": {}, "PublishAllPorts": False, "NetworkMode": primary},
            "Mounts": [],
            "NetworkSettings": {"Networks": dict.fromkeys(networks), "Ports": None},
        }

    runner = container(1001, JOBS, [JOBS])
    runner["HostConfig"].update(Binds=[f"{STATE}:/state:rw"],
                                Mounts=[{"Type": "volume", "Target": "/runner"}],
                                Tmpfs={"/tmp": "rw,nosuid", "/home/runner": "rw,nosuid"})
    runner["Mounts"] = [volume(STATE, "/state"), volume(SCRATCH, "/runner")]
    egress = container(65532, OUTSIDE, [JOBS, OUTSIDE])
    egress["NetworkSettings"]["Ports"] = {"3128/tcp": None}
    egress["HostConfig"]["Tmpfs"] = {"/tmp": "rw,nosuid,nodev,noexec"}
    volumes = {
        m["Name"]: {"Name": m["Name"], "Driver": "local", "Scope": "local", "Options": None,
                    "Mountpoint": m["Source"], "Labels": None}
        for m in runner["Mounts"]
    }
    volumes[STATE]["Labels"] = {"com.docker.compose.project": PROJECT, "com.docker.compose.volume": "state"}
    return {"runner": runner, "egress": egress}, volumes


class ContainmentTests(unittest.TestCase):
    def check(self, containers, volumes):
        def engine_json(*argv):
            cli, name = argv[:-1], argv[-1]
            self.assertEqual(cli, preflight.DOCKER + ("volume", "inspect"))
            return [volumes[name]]

        with patch.object(preflight, "read_json", side_effect=engine_json):
            preflight.check_containers(containers, PROJECT)

    def test_legacy_and_typed_docker_volume_encodings(self):
        containers, volumes = fixture()
        self.check(containers, volumes)
        runner = containers["runner"]
        runner["HostConfig"]["Binds"] = []
        runner["HostConfig"]["Mounts"].append({"Type": "volume", "Source": STATE, "Target": "/state"})
        runner["Mounts"].append({"Type": "tmpfs", "Destination": "/tmp"})
        volumes[SCRATCH]["Options"] = {}
        self.check(containers, volumes)

    def test_real_host_bind_is_refused_even_at_expected_target(self):
        for source, target in (("/opt/host-data", "/state"), ("/var/run/docker.sock", "/var/run/docker.sock")):
            for typed in (False, True):
                with self.subTest(source=source, target=target, typed=typed):
                    containers, volumes = fixture()
                    runner = containers["runner"]
                    runner["Mounts"] = [m for m in runner["Mounts"] if m["Destination"] != target]
                    runner["Mounts"].append({"Type": "bind", "Source": source, "Destination": target})
                    if typed:
                        runner["HostConfig"]["Mounts"].append({"Type": "bind", "Source": source, "Target": target})
                    else:
                        runner["HostConfig"]["Binds"].append(f"{source}:{target}:rw")
                    with self.assertRaisesRegex(RuntimeError, "unexpected effective mount"):
                        self.check(containers, volumes)

    def test_raw_legacy_or_typed_bind_cannot_be_hidden_by_expected_volume_metadata(self):
        containers, volumes = fixture()
        for key, entry, error in (
            ("Binds", "/var/run/docker.sock:/runner:rw", "legacy bind entry"),
            ("Mounts", {"Type": "bind", "Source": "/var/run/docker.sock", "Target": "/runner"}, "structured mount"),
        ):
            with self.subTest(key=key):
                modified = deepcopy(containers)
                modified["runner"]["HostConfig"][key].append(entry)
                with self.assertRaisesRegex(RuntimeError, error):
                    self.check(modified, volumes)

    def test_local_driver_bind_and_wrong_volume_identity_are_refused(self):
        containers, volumes = fixture()
        for name in (STATE, SCRATCH):
            with self.subTest(name=name):
                modified = deepcopy(volumes)
                modified[name]["Options"] = {"type": "none", "o": "bind", "device": "/home/host"}
                with self.assertRaisesRegex(RuntimeError, "driver options and host backing are forbidden"):
                    self.check(containers, modified)
        for index, msg in ((0, "test state volume"), (1, "anonymous Docker volume")):
            with self.subTest(index=index):
                modified = deepcopy(containers)
                modified["runner"]["HostConfig"].update(Binds=None, Mounts=[])
                modified["runner"]["Mounts"][index]["Name"] = "different-named-volume"
                with self.assertRaisesRegex(RuntimeError, msg):
                    self.check(modified, volumes)
        volumes[STATE]["Labels"]["com.docker.compose.project"] = "different-project"
        with self.assertRaisesRegex(RuntimeError, "does not belong to this test project"):
            self.check(containers, volumes)

    def test_no_published_ports_or_other_networks(self):
        assigned = {"3128/tcp": [{"HostIp": "127.0.0.1", "HostPort": "3128"}]}
        for field, key, bad, error in (
            ("HostConfig", "PublishAllPorts", True, "must not publish ports"),
            ("HostConfig", "PortBindings", assigned, "must not publish ports"),
            ("NetworkSettings", "Ports", assigned, "must not publish ports"),
            ("HostConfig", "NetworkMode", "host", "unexpected network"),
            ("NetworkSettings", "Networks", dict.fromkeys([JOBS, OUTSIDE]), "unexpected network"),
        ):
            with self.subTest(field=field, key=key):
                containers, volumes = fixture()
                containers["runner"][field][key] = bad
                with self.assertRaisesRegex(RuntimeError, error):
                    self.check(containers, volumes)


if __name__ == "__main__":
    unittest.main()
