#!/usr/bin/python3
"""Writes LibreOffice Writer's accept-all and reject-all text of a .docx.

Usage: /usr/bin/python3 libreoffice-oracle.py FILE.docx PROFILE_DIR

Needs LibreOffice Writer and its Python bridge (python3-uno), so it runs with the
system Python rather than uv. Output: FILE.accepted.txt and FILE.rejected.txt.
"""
import subprocess, sys, time, uno
from com.sun.star.beans import PropertyValue

def prop(name, value):
    p = PropertyValue(); p.Name = name; p.Value = value; return p

office = subprocess.Popen(["soffice", "-env:UserInstallation=file://" + sys.argv[2], "--headless", "--norestore",
                           "--accept=socket,host=127.0.0.1,port=2099;urp;"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
local = uno.getComponentContext()
resolver = local.ServiceManager.createInstanceWithContext("com.sun.star.bridge.UnoUrlResolver", local)
for _ in range(60):
    try:
        ctx = resolver.resolve("uno:socket,host=127.0.0.1,port=2099;urp;StarOffice.ComponentContext"); break
    except Exception:
        time.sleep(1)
smgr = ctx.ServiceManager
desktop = smgr.createInstanceWithContext("com.sun.star.frame.Desktop", ctx)
dispatcher = smgr.createInstanceWithContext("com.sun.star.frame.DispatchHelper", ctx)
url = uno.systemPathToFileUrl(sys.argv[1])
for command in ("AcceptAllTrackedChanges", "RejectAllTrackedChanges"):
    doc = desktop.loadComponentFromURL(url, "_blank", 0, (prop("Hidden", True),))
    dispatcher.executeDispatch(doc.getCurrentController().getFrame(), ".uno:" + command, "", 0, ())
    suffix = "accepted" if command.startswith("Accept") else "rejected"
    with open(sys.argv[1].removesuffix(".docx") + f".{suffix}.txt", "w", encoding="utf-8") as out:
        out.write(doc.getText().getString().replace("\r\n", "\n") + "\n")
    doc.close(True)
office.terminate()
