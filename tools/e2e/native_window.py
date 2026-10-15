#!/usr/bin/env python3
"""Drive the actual packaged system WebView, real Controller and independently supervised Agent."""
import http.cookiejar
import json
import os
from pathlib import Path
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

from native_local import AGENT, ROOT, forwarded, probe, stop_owned, wait, Echo


def startup_probe(executable, base_env, folder, output, mode, startup_hidden=True):
    report = folder / f"{mode}.json"
    env = {**base_env, "RUSTFRP_NATIVE_E2E_MODE": mode, "RUSTFRP_NATIVE_E2E_REPORT": str(report)}
    env.pop("RUSTFRP_DESKTOP_STATE_DIR", None)
    env.pop("LOCALAPPDATA", None)
    home = folder / f"{mode}-home"
    home.mkdir(mode=0o700)
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    if mode == "isolated-startup":
        env["RUSTFRP_DESKTOP_STATE_DIR"] = str(folder / "isolated-state")
    if mode == "no-tray-startup":
        env["RUSTFRP_NATIVE_E2E_NO_TRAY"] = "1"
    with output.open("a") as log:
        command = [str(executable)] + (["--startup-to-tray"] if startup_hidden else [])
        app = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 25
            while time.monotonic() < deadline:
                if report.exists():
                    evidence = json.loads(report.read_text())
                    assert evidence.get("passed") and evidence.get("stage") == mode, evidence
                    assert app.wait(timeout=5) == 0
                    return evidence
                if app.poll() is not None:
                    raise AssertionError(f"{mode} startup probe exited before reporting")
                time.sleep(0.1)
            raise AssertionError(f"{mode} startup probe timed out")
        finally:
            if app.poll() is None:
                app.terminate()
                try: app.wait(timeout=5)
                except subprocess.TimeoutExpired: app.kill(); app.wait(timeout=5)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("Usage: native_window.py <native-e2e application executable>")
    executable = Path(sys.argv[1]).resolve()
    assert executable.is_file()
    cookie = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookie))
    csrf = ""
    def api(method, path, body=None):
        headers = {"Origin":"http://127.0.0.1:9674", "Content-Type":"application/json", "x-rustfrp-admin":"1", "x-csrf-token":csrf}
        request = urllib.request.Request("http://127.0.0.1:9674/api/v1"+path, data=None if body is None else json.dumps(body).encode(), method=method, headers=headers)
        with opener.open(request, timeout=15) as response:
            raw = response.read(2*1024*1024+1)
            assert len(raw)<=2*1024*1024
            return json.loads(raw) if raw else None
    password=os.environ.get("RUSTFRP_E2E_PASSWORD","native ui test password")
    api("POST","/auth/login",{"name":"preview","password":password})
    csrf=api("GET","/auth/me")["csrf"]
    node=api("GET","/node")
    name=f"native-window-e2e-{time.time_ns()}"
    token=api("POST","/enroll-tokens",{"name":name,"ttl_minutes":5,"reason":"Actual WebView E2E"})
    with tempfile.TemporaryDirectory(prefix="rustfrp-native-window-") as temporary, socketserver.TCPServer(("127.0.0.1",0), Echo) as echo:
        folder=Path(temporary)
        home=folder/"home"
        home.mkdir(mode=0o700)
        root=home/".rustfrp"/"native-desktop"
        root.mkdir(parents=True,mode=0o700)
        missing_supervisor=subprocess.run([str(executable),"--agent-supervisor",str(folder/"missing-job.json")],capture_output=True,text=True,timeout=10)
        assert missing_supervisor.returncode!=0 and "后台" in missing_supervisor.stderr
        extra_supervisor=subprocess.run([str(executable),"--agent-supervisor",str(folder/"missing-job.json"),"unexpected"],capture_output=True,text=True,timeout=10)
        assert extra_supervisor.returncode!=0 and "参数" in extra_supervisor.stderr
        relative_state = subprocess.run([str(executable)], env={**os.environ, "RUSTFRP_DESKTOP_STATE_DIR": "relative"}, capture_output=True, text=True, timeout=10)
        assert relative_state.returncode != 0 and "绝对路径" in relative_state.stderr
        local={"binary":str(root/"bin"/AGENT.name),"stateDir":str(folder/"agent"),"name":name}
        preferences=root/"preferences.json"
        preferences.write_text(json.dumps({"local":local,"controller_url":"http://127.0.0.1:9674","notifications":False,"lightweight":True}))
        if os.name!="nt": preferences.chmod(0o600)
        report=folder/"report.json"
        env={**os.environ,"HOME":str(home),"LOCALAPPDATA":str(home),"USERPROFILE":str(home),"RUSTFRP_E2E_PASSWORD":password,"RUSTFRP_NATIVE_E2E_TOKEN":token["token"],"RUSTFRP_NATIVE_E2E_NAME":name,"RUSTFRP_NATIVE_E2E_REPORT":str(report)}
        env.pop("RUSTFRP_DESKTOP_STATE_DIR",None)
        thread=threading.Thread(target=echo.serve_forever,daemon=True); thread.start()
        evidence={}
        single_instance=False
        client_id=None
        output=ROOT/"target/ui/native-window.log"
        output.parent.mkdir(parents=True,exist_ok=True)
        with output.open("w") as log:
            app=subprocess.Popen([str(executable),"--startup-to-tray"],env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log)
            try:
                deadline=time.monotonic()+200
                previous=""
                while time.monotonic()<deadline:
                    if report.exists():
                        evidence=json.loads(report.read_text())
                        stage=evidence["stage"]
                        if stage!=previous: print(f"Native WebView: {stage}",flush=True); previous=stage
                        if not evidence.get("passed"): raise AssertionError(evidence.get("message","native test failed"))
                        if stage=="local-started" and not single_instance:
                            second=subprocess.Popen([str(executable),"--e2e-secondary"],env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log)
                            try: assert second.wait(timeout=5)==0
                            finally:
                                if second.poll() is None: second.terminate(); second.wait(timeout=5)
                            assert app.poll() is None
                            single_instance=True
                        if stage=="restored": break
                    if app.poll() is not None: raise AssertionError("Native app exited before integration test completed")
                    time.sleep(0.25)
                else: raise AssertionError("Native WebView E2E timed out; inspect target/ui/native-window.log")
                assert app.wait(timeout=5)==0
                current=probe(root)
                assert current["status"]["pid"]==evidence["agent_pid"] and current["status"]["connected"]
                client_id=next(item["id"] for item in api("GET","/clients") if item["name"]==name)
                tunnel=api("POST","/tunnels",{"name":"native-window-echo","kind":"tcp","client_id":client_id,"local_ip":"127.0.0.1","local_port":echo.server_address[1],"remote_port":0,"enabled":True,"reason":"Actual application exit forwarding E2E"})
                current=wait(lambda: item if (item:=api("GET",f"/tunnels/{tunnel['id']}"))["state"]=="running" else None)
                wait(lambda:forwarded(current["status_port"]))
                assert single_instance
                evidence.update({"platform":sys.platform,"actual_system_webview":True,"single_instance":True,"agent_survives_application_exit":True,"forwarding_after_application_exit":True})
            finally:
                if app.poll() is None:
                    app.terminate()
                    try: app.wait(timeout=5)
                    except subprocess.TimeoutExpired: app.kill(); app.wait(timeout=5)
                stop_owned(root,local)
                if not client_id: client_id=next((item["id"] for item in api("GET","/clients") if item["name"]==name),None)
                if client_id: api("DELETE",f"/clients/{client_id}?reason=Native%20window%20E2E%20cleanup")
                for item in api("GET","/api-tokens"):
                    if item["name"]==name and item.get("revoked_at") is None: api("POST",f"/api-tokens/{item['id']}/revoke",{"reason":"Native window E2E credential cleanup"})
                echo.shutdown()
        evidence["relative_state_rejected"] = True
        evidence["isolated_startup"] = startup_probe(executable, env, folder, output, "isolated-startup")
        evidence["home_startup"] = startup_probe(executable, env, folder, output, "home-startup", startup_hidden=False)
        evidence["no_tray_startup"] = startup_probe(executable, env, folder, output, "no-tray-startup")
    (ROOT/"target/ui/native-window-e2e.json").write_text(json.dumps(evidence,indent=2)+"\n")
    print(json.dumps(evidence,indent=2))


if __name__=="__main__": main()
