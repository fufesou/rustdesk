on run {daemon_file, agent_file, user, cur_pid, source_dir}

  set agent_plist to "/Library/LaunchAgents/com.carriez.RustDesk_server.plist"
  set daemon_plist to "/Library/LaunchDaemons/com.carriez.RustDesk_service.plist"
  set app_bundle to "/Applications/RustDesk.app"

  set unload_agent to "launchctl unload -w " & quoted form of agent_plist & " || true;"
  set unload_service to "launchctl unload -w " & quoted form of daemon_plist & " || true;"
  set kill_others to "pids=$(pgrep -x 'RustDesk' | grep -vx " & quoted form of cur_pid & " || true); if [ -n \"$pids\" ]; then echo \"$pids\" | xargs kill -9 || true; fi;"

  set copy_files to "rm -rf " & quoted form of app_bundle & " && ditto " & quoted form of source_dir & " " & quoted form of app_bundle & " && chown -R " & quoted form of user & ":staff " & quoted form of app_bundle & " && (xattr -r -d com.apple.quarantine " & quoted form of app_bundle & " || true);"

  set write_daemon_plist to "echo " & quoted form of daemon_file & " > " & quoted form of daemon_plist & " && chown root:wheel " & quoted form of daemon_plist & ";"
  set write_agent_plist to "echo " & quoted form of agent_file & " > " & quoted form of agent_plist & " && chown root:wheel " & quoted form of agent_plist & ";"
  set load_service to "launchctl load -w " & quoted form of daemon_plist & ";"

  set sh to "set -e;" & unload_agent & unload_service & kill_others & copy_files & write_daemon_plist & write_agent_plist & load_service

  do shell script sh with prompt "RustDesk wants to update itself" with administrator privileges
end run
