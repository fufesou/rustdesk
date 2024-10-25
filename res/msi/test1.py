import os
import subprocess
import shlex


def msi_build(app_name0, conn_type):
    # no idea why msbuild can not be found in pm2, though can be found in "python -i"
    # on Windows, os.system is cmd rather than bash, here need to use windows path for command
    # os.system(
    #     '"c:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\MSBuild\\Current\\Bin\\msbuild.exe" /t:clean'
    # )
    os.system(
        "git restore ."
    )
    conn_type_param = "" if not conn_type else "--conn-type %s" % conn_type
    os.system(
        "python preprocess.py --arp -d ../../rustdesk --version 1.3.0.1 --app-name %s %s"
        % (app_name0, conn_type_param)
    )
    os.system(
        '"c:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\MSBuild\\Current\\Bin\\msbuild.exe" msi.sln -p:Configuration=Release -p:Platform=x64 /p:TargetVersion=Windows10'
    )


msi_build("rustdesk", '')
