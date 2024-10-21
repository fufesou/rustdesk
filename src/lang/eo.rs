lazy_static::lazy_static! {
pub static ref T: std::collections::HashMap<&'static str, &'static str> =
    [
        ("Status", "Stato"),
        ("Your Desktop", "Via aparato"),
        ("desk_tip", "Via aparato povas esti alirita kun tiu identigilo kaj pasvorto"),
        ("Password", "Pasvorto"),
        ("Ready", "Preta"),
        ("Established", "Establis"),
        ("connecting_status", "Konektante al la reto RustDesk..."),
        ("Enable service", "Ebligi servon"),
        ("Start service", "Starti servon"),
        ("Service is running", "La servo funkcias"),
        ("Service is not running", "La servo ne funkcias"),
        ("not_ready_status", "Ne preta, bonvolu kontroli la retkonekto"),
        ("Control Remote Desktop", "Kontroli foran aparaton"),
        ("Transfer file", "Transigi dosieron"),
        ("Connect", "Konekti al"),
        ("Recent sessions", "Lastaj sesioj"),
        ("Address book", "Adresaro"),
        ("Confirmation", "Konfirmacio"),
        ("TCP tunneling", "Tunelado TCP"),
        ("Remove", "Forigi"),
        ("Refresh random password", "Regeneri hazardan pasvorton"),
        ("Set your own password", "Agordi vian propran pasvorton"),
        ("Enable keyboard/mouse", "Ebligi klavaro/muso"),
        ("Enable clipboard", "Sinkronigi poŝon"),
        ("Enable file transfer", "Ebligi dosiertransigado"),
        ("Enable TCP tunneling", "Ebligi tunelado TCP"),
        ("IP Whitelisting", "Listo de IP akceptataj"),
        ("ID/Relay Server", "Identigila/Relajsa servilo"),
        ("Import server config", "Importi servilan agordon"),
        ("Export Server Config", "Eksporti servilan agordon"),
        ("Import server configuration successfully", "Importi servilan agordon sukcese"),
        ("Export server configuration successfully", "Eksporti servilan agordon sukcese"),
        ("Invalid server configuration", "Nevalida servila agordo"),
        ("Clipboard is empty", "La poŝo estas malplena"),
        ("Stop service", "Haltu servon"),
        ("Change ID", "Ŝanĝi identigilon"),
        ("Your new ID", "Via nova identigilo"),
        ("length %min% to %max%", "longeco %min% al %max%"),
        ("starts with a letter", "komencas kun letero"),
        ("allowed characters", "permesitaj signoj"),
        ("id_change_tip", "Nur la signoj a-z, A-Z, 0-9, _ (substreko) povas esti uzataj. La unua litero povas esti inter a-z, A-Z. La longeco devas esti inter 6 kaj 16."),
        ("Website", "Retejo"),
        ("About", "Pri"),
        ("Slogan_tip", "Farita kun koro en ĉi tiu ĥaosa mondo!"),
        ("Privacy Statement", "Deklaro Pri Privateco"),
        ("Mute", "Muta"),
        ("Build Date", "konstruada dato"),
        ("Version", "Versio"),
        ("Home", "Hejmo"),
        ("Audio Input", "Aŭdia Enigo"),
        ("Enhancements", "Plibonigoj"),
        ("Hardware Codec", "Aparataro Kodeko"),
        ("Adaptive bitrate", "Adapta bitrapido"),
        ("ID Server", "Servilo de identigiloj"),
        ("Relay Server", "Relajsa Servilo"),
        ("API Server", "Servilo de API"),
        ("invalid_http", "Devas komenci kun http:// aŭ https://"),
        ("Invalid IP", "IP nevalida"),
        ("Invalid format", "Formato nevalida"),
        ("server_not_support", "Ankoraŭ ne subtenata de la servilo"),
        ("Not available", "Nedisponebla"),
        ("Too frequent", "Tro ofte ŝanĝita, bonvolu reprovi poste"),
        ("Cancel", "Nuligi"),
        ("Skip", "Ignori"),
        ("Close", "Fermi"),
        ("Retry", "Reprovi"),
        ("OK", "Konfermi"),
        ("Password Required", "Pasvorto deviga"),
        ("Please enter your password", "Bonvolu tajpi vian pasvorton"),
        ("Remember password", "Memori pasvorton"),
        ("Wrong Password", "Erara pasvorto"),
        ("Do you want to enter again?", "Ĉu vi aliri denove?"),
        ("Connection Error", "Eraro de konektado"),
        ("Error", "Eraro"),
        ("Reset by the peer", "La konekto estas fermita de la samtavolano"),
        ("Connecting...", "Konektante..."),
        ("Connection in progress. Please wait.", "Konektado farata. Bonvolu atendi."),
        ("Please try 1 minute later", "Reprovi post 1 minuto"),
        ("Login Error", "Eraro de konektado"),
        ("Successful", "Sukceso"),
        ("Connected, waiting for image...", "Konektita, atendante bildon..."),
        ("Name", "Nomo"),
        ("Type", "Tipo"),
        ("Modified", "Modifita"),
        ("Size", "Grandeco"),
        ("Show Hidden Files", "Montri kaŝitajn dosierojn"),
        ("Receive", "Akcepti"),
        ("Send", "Sendi"),
        ("Refresh File", "Aktualigu Dosieron"),
        ("Local", "Loka"),
        ("Remote", "Fora"),
        ("Remote Computer", "Fora komputilo"),
        ("Local Computer", "Loka komputilo"),
        ("Confirm Delete", "Konfermi la forigo"),
        ("Delete", "Forigi"),
        ("Properties", "Propraĵoj"),
        ("Multi Select", "Pluropa Elekto"),
        ("Select All", "Elektu Ĉiujn"),
        ("Unselect All", "Malelektu Ĉiujn"),
        ("Empty Directory", "Malplena Dosierujo"),
        ("Not an empty directory", "Ne Malplena Dosierujo"),
        ("Are you sure you want to delete this file?", "Ĉu vi certas, ke vi volas forigi ĉi tiun dosieron?"),
        ("Are you sure you want to delete this empty directory?", "Ĉu vi certas, ke vi volas forigi ĉi tiun malplenan dosierujon?"),
        ("Are you sure you want to delete the file of this directory?", "Ĉu vi certa. ke vi volas forigi la dosieron de ĉi tiu dosierujo"),
        ("Do this for all conflicts", "Same por ĉiuj konfliktoj"),
        ("This is irreversible!", "Ĉi tio estas neinversigebla!"),
        ("Deleting", "Forigado"),
        ("files", "dosiero"),
        ("Waiting", "Atendante..."),
        ("Finished", "Finita"),
        ("Speed", "Rapideco"),
        ("Custom Image Quality", "Agordi bildan kvaliton"),
        ("Privacy mode", "Modo privata"),
        ("Block user input", "Bloki uzanta enigo"),
        ("Unblock user input", "Malbloki uzanta enigo"),
        ("Adjust Window", "Adapti fenestro"),
        ("Original", "Originala rilatumo"),
        ("Shrink", "Ŝrumpi"),
        ("Stretch", "Streĉi"),
        ("Scrollbar", "Rulumbreto"),
        ("ScrollAuto", "Rulumu Aŭtomate"),
        ("Good image quality", "Bona bilda kvalito"),
        ("Balanced", "Normala bilda kvalito"),
        ("Optimize reaction time", "Optimigi reakcia tempo"),
        ("Custom", ""),
        ("Show remote cursor", "Montri foran kursoron"),
        ("Show quality monitor", "Montri kvalito monitoron"),
        ("Disable clipboard", "Malebligi poŝon"),
        ("Lock after session end", "Ŝlosi foran komputilon post malkonektado"),
        ("Insert", "Enmeti"),
        ("Insert Lock", "Ŝlosi foran komputilon"),
        ("Refresh", "Refreŝigi ekranon"),
        ("ID does not exist", "La identigilo ne ekzistas"),
        ("Failed to connect to rendezvous server", "Malsukcesis konekti al la servilo rendezvous"),
        ("Please try later", "Bonvolu provi poste"),
        ("Remote desktop is offline", "La fora aparato estas senkonektita"),
        ("Key mismatch", "Miskongruo de klavoj"),
        ("Timeout", "Konekta posttempo"),
        ("Failed to connect to relay server", "Malsukcesis konekti al la relajsa servilo"),
        ("Failed to connect via rendezvous server", "Malsukcesis konekti per servilo rendezvous"),
        ("Failed to connect via relay server", "Malsukcesis konekti per relajsa servilo"),
        ("Failed to make direct connection to remote desktop", "Malsukcesis konekti direkte"),
        ("Set Password", "Agordi pasvorton"),
        ("OS Password", "Pasvorto de la operaciumo"),
        ("install_tip", "Vi ne uzas instalita versio. Pro limigoj pro UAC, kiel aparato kontrolata, en kelkaj kazoj, ne estos ebla kontroli la muson kaj klavaron aŭ registri la ekranon. Bonvolu alkliku la butonon malsupre por instali RustDesk sur la operaciumo por eviti la demando supre."),
        ("Click to upgrade", "Alklaki por plibonigi"),
        ("Click to download", "Alklaki por elŝuti"),
        ("Click to update", "Alklaki por ĝisdatigi"),
        ("Configure", "Konfiguri"),
        ("config_acc", "Por uzi vian foran aparaton, bonvolu doni la permeson \"alirebleco\" al RustDesk."),
        ("config_screen", "Por uzi vian foran aparaton, bonvolu doni la permeson \"ekranregistrado\" al RustDesk."),
        ("Installing ...", "Instalante..."),
        ("Install", "Instali"),
        ("Installation", "Instalado"),
        ("Installation Path", "Vojo de instalo"),
        ("Create start menu shortcuts", "Aldoni ligilojn sur la startmenuo"),
        ("Create desktop icon", "Aldoni ligilojn sur la labortablo"),
        ("agreement_tip", "Starti la instaladon signifas akcepti la permesilon."),
        ("Accept and Install", "Akcepti kaj instali"),
        ("End-user license agreement", "Uzanta permesilon"),
        ("Generating ...", "Generante..."),
        ("Your installation is lower version.", "Via versio de instalaĵo estas pli malalta ol la lasta."),
        ("not_close_tcp_tip", "Bonvolu ne fermu tiun fenestron dum la uzo de la tunelo"),
        ("Listening ...", "Atendante konekton al la tunelo..."),
        ("Remote Host", "Fora gastiganto"),
        ("Remote Port", "Fora pordo"),
        ("Action", "Ago"),
        ("Add", "Aldoni"),
        ("Local Port", "Loka pordo"),
        ("Local Address", "Loka Adreso"),
        ("Change Local Port", "Ŝanĝi Loka Pordo"),
        ("setup_server_tip", "Se vi bezonas pli rapida konekcio, vi povas krei vian propran servilon"),
        ("Too short, at least 6 characters.", "Tro mallonga, almenaŭ 6 signoj."),
        ("The confirmation is not identical.", "Ambaŭ enigoj ne kongruas"),
        ("Permissions", "Permesoj"),
        ("Accept", "Akcepti"),
        ("Dismiss", "Malakcepti"),
        ("Disconnect", "Malkonekti"),
        ("Enable file copy and paste", "Permesu kopii kaj alglui dosierojn"),
        ("Connected", "Konektata"),
        ("Direct and encrypted connection", "Konekcio direkta ĉifrata"),
        ("Relayed and encrypted connection", "Konekcio relajsa ĉifrata"),
        ("Direct and unencrypted connection", "Konekcio direkta neĉifrata"),
        ("Relayed and unencrypted connection", "Konekcio relajsa neĉifrata"),
        ("Enter Remote ID", "Tajpu foran identigilon"),
        ("Enter your password", "Tajpu vian pasvorton"),
        ("Logging in...", "Konektante..."),
        ("Enable RDP session sharing", "Ebligi la kundivido de sesio RDP"),
        ("Auto Login", "Aŭtomata konektado (la ŝloso nur estos ebligita post la malebligado de la unua parametro)"),
        ("Enable direct IP access", "Permesi direkta eniro per IP"),
        ("Rename", "Renomi"),
        ("Space", "Spaco"),
        ("Create desktop shortcut", "Krei ligilon sur la labortablon"),
        ("Change Path", "Ŝanĝi vojon"),
        ("Create Folder", "Krei dosierujon"),
        ("Please enter the folder name", "Bonvolu enigi la dosiernomon"),
        ("Fix it", "Riparu ĝin"),
        ("Warning", "Averto"),
        ("Login screen using Wayland is not supported", "Konektajn ekranojn uzantajn Wayland ne estas subtenitaj"),
        ("Reboot required", "Restarto deviga"),
        ("Unsupported display server", "La aktuala bilda servilo ne estas subtenita"),
        ("x11 expected", "Bonvolu uzi x11"),
        ("Port", "Pordo"),
        ("Settings", "Agordoj"),
        ("Username", " Uzanta nomo"),
        ("Invalid port", "Pordo nevalida"),
        ("Closed manually by the peer", "Manuale fermita de la samtavolano"),
        ("Enable remote configuration modification", "Permesi foran redaktadon de la konfiguracio"),
        ("Run without install", "Plenumi sen instali"),
        ("Connect via relay", "Konekti per relajso"),
        ("Always connect via relay", "Ĉiam konekti per relajso"),
        ("whitelist_tip", "Nur la IP en la blanka listo povas kontroli mian komputilon"),
        ("Login", "Ensaluti"),
        ("Verify", "Kontrolis"),
        ("Remember me", "Memori min"),
        ("Trust this device", "Fidu ĉi tiun aparaton"),
        ("Verification code", "Konfirmkodo"),
        ("verification_tip", "Konfirmkodo estis sendita al la registrita retpoŝta adreso, enigu la konfirmkodon por daŭrigi ensaluti."),
        ("Logout", "Elsaluti"),
        ("Tags", "Etikedi"),
        ("Search ID", "Serĉi ID"),
        ("whitelist_sep", "Vi povas uzi komon, punktokomon, spacon aŭ linsalton kiel apartigilo"),
        ("Add ID", "Aldoni identigilo"),
        ("Add Tag", "Aldoni etikedo"),
        ("Unselect all tags", "Malselekti ĉiujn etikedojn"),
        ("Network error", "Reta eraro"),
        ("Username missed", "Uzantnomo forgesita"),
        ("Password missed", "Pasvorto forgesita"),
        ("Wrong credentials", "Identigilo aŭ pasvorto erara"),
        ("The verification code is incorrect or has expired", ""),
        ("Edit Tag", "Redakti etikedo"),
        ("Forget Password", "Forgesi pasvorton"),
        ("Favorites", "Favorataj"),
        ("Add to Favorites", "Aldoni al la favorataj"),
        ("Remove from Favorites", "Forigi el la favorataj"),
        ("Empty", "Malplena"),
        ("Invalid folder name", "Dosiernomo nevalida"),
        ("Socks5 Proxy", "Socks5 prokura servilo"),
        ("Socks5/Http(s) Proxy", "Socks5/Http(s) prokura servilo"),
        ("Discovered", "Malkovritaj"),
        ("install_daemon_tip", "Por komenci ĉe ekŝargo, oni devas instali sisteman servon."),
        ("Remote ID", "Fora identigilo"),
        ("Paste", "Alglui"),
        ("Paste here?", "Alglui ĉi tie?"),
        ("Are you sure to close the connection?", "Ĉu vi vere volas fermi la konekton?"),
        ("Download new version", "Elŝuti la novan version"),
        ("Touch mode", "Tuŝa modo"),
        ("Mouse mode", "musa modo"),
        ("One-Finger Tap", "Unufingra Frapeto"),
        ("Left Mouse", "Maldekstra Muso"),
        ("One-Long Tap", "Unulonga Frapeto"),
        ("Two-Finger Tap", "Dufingra Frapeto"),
        ("Right Mouse", "Deskra Muso"),
        ("One-Finger Move", "Unufingra Movo"),
        ("Double Tap & Move", "Duobla Frapeto & Movo"),
        ("Mouse Drag", "Muso Trenadi"),
        ("Three-Finger vertically", "Tri Figroj Vertikale"),
        ("Mouse Wheel", "Musa Rado"),
        ("Two-Finger Move", "Dufingra Movo"),
        ("Canvas Move", "Kanvasa Movo"),
        ("Pinch to Zoom", "Pinĉi al Zomo"),
        ("Canvas Zoom", "Kanvasa Zomo"),
        ("Reset canvas", "Restarigi kanvaso"),
        ("No permission of file transfer", "Neniu permeso de dosiertransigo"),
        ("Note", "Notu"),
        ("Connection", "Konekto"),
        ("Share Screen", "Kunhavigi Ekranon"),
        ("Chat", "Babilo"),
        ("Total", "Sumo"),
        ("items", "eroj"),
        ("Selected", "Elektita"),
        ("Screen Capture", "Ekrankapto"),
        ("Input Control", "Eniga Kontrolo"),
        ("Audio Capture", "Sonkontrolo"),
        ("File Connection", "Dosiero Konekto"),
        ("Screen Connection", "Ekrono konekto"),
        ("Do you accept?", "Ĉu vi akceptas?"),
        ("Open System Setting", "Malfermi Sistemajn Agordojn"),
        ("How to get Android input permission?", "Kiel akiri Android enigajn permesojn"),
        ("android_input_permission_tip1", "Por ke fora aparato regu vian Android-aparaton per muso aŭ tuŝo, vi devas permesi al RustDesk uzi la servon \"Alirebleco\"."),
        ("android_input_permission_tip2", "Bonvolu iri al la sekva paĝo de sistemaj agordoj, trovi kaj eniri [Instatajn Servojn], ŝalti la servon [RustDesk Enigo]."),
        ("android_new_connection_tip", "Nova kontrolpeto estis ricevita, kiu volas kontroli vian nunan aparaton."),
        ("android_service_will_start_tip", "Ŝalti \"Ekrankapto\" aŭtomate startos la servon, permesante al aliaj aparatoj peti konekton al via aparato."),
        ("android_stop_service_tip", "Fermante la servon aŭtomate fermos ĉiujn establitajn konektojn."),
        ("android_version_audio_tip", "La nuna versio da Android ne subtenas sonkapton, bonvolu ĝisdatigi al Android 10 aŭ pli alta."),
        ("android_start_service_tip", "Frapu [Komenci servo] aŭ ebligu la permeson de [Ekrankapto] por komenci la servon de kundivido de ekrano."),
        ("android_permission_may_not_change_tip", "Permesoj por establitaj konektoj neble estas ŝanĝitaj tuj ĝis rekonektitaj."),
        ("Account", "Konto"),
        ("Overwrite", "anstataŭigi"),
        ("This file exists, skip or overwrite this file?", "Ĉi tiu dosiero ekzistas, ĉu preterlasi aŭ anstataŭi ĉi tiun dosieron?"),
        ("Quit", "Forlasi"),
        ("Help", "Helpi"),
        ("Failed", "Malsukcesa"),
        ("Succeeded", "Sukcesa"),
        ("Someone turns on privacy mode, exit", "Iu ŝaltas modon privata, Eliro"),
        ("Unsupported", "Nesubtenata"),
        ("Peer denied", "Samulo rifuzita"),
        ("Please install plugins", "Bonvolu instali kromprogramojn"),
        ("Peer exit", "Samulo eliras"),
        ("Failed to turn off", "Malsukcesis malŝalti"),
        ("Turned off", "Malŝaltita"),
        ("Language", "Lingvo"),
        ("Keep RustDesk background service", "Tenu RustDesk fonan servon"),
        ("Ignore Battery Optimizations", "Ignoru Bateria Optimumigojn"),
        ("android_open_battery_optimizations_tip", "Se vi volas malŝalti ĉi tiun funkcion, bonvolu iri al la sekva paĝo de agordoj de la aplikaĵo de RustDesk, trovi kaj eniri [Baterio], Malmarku [Senrestrikta]"),
        ("Start on boot", "Komencu ĉe ekfunkciigo"),
        ("Start the screen sharing service on boot, requires special permissions", "Komencu la servon de kundivido de ekrano ĉe lanĉo, postulas specialajn permesojn"),
        ("Connection not allowed", "Konekto ne rajtas"),
        ("Legacy mode", ""),
        ("Map mode", "Mapa modo"),
        ("Translate mode", "Traduki modo"),
        ("Use permanent password", "Uzu permanenta pasvorto"),
        ("Use both passwords", "Uzu ambaŭ pasvorto"),
        ("Set permanent password", "Starigi permanenta pasvorto"),
        ("Enable remote restart", "Permesi fora restartas"),
        ("Restart remote device", "Restartu fora aparato"),
        ("Are you sure you want to restart", "Ĉu vi certas, ke vi volas restarti"),
        ("Restarting remote device", "Restartas fora aparato"),
        ("remote_restarting_tip", "Fora aparato restartiĝas, bonvolu fermi ĉi tiun mesaĝkeston kaj rekonekti kun permanenta pasvorto post iom da tempo"),
        ("Copied", "Kopiita"),
        ("Exit Fullscreen", "Eliru Plenekranon"),
        ("Fullscreen", "Plenekrane"),
        ("Mobile Actions", "Poŝtelefonaj Agoj"),
        ("Select Monitor", "Elektu Monitoron"),
        ("Control Actions", "Kontrolaj Agoj"),
        ("Display Settings", "Montraj Agordoj"),
        ("Ratio", "Proporcio"),
        ("Image Quality", "Bilda Kvalito"),
        ("Scroll Style", "Ruluma Stilo"),
        ("Show Toolbar", "Montri Ilobreton"),
        ("Hide Toolbar", "Kaŝi Ilobreton"),
        ("Direct Connection", "Rekta Konekto"),
        ("Relay Connection", "Relajsa Konekto"),
        ("Secure Connection", "Sekura Konekto"),
        ("Insecure Connection", "Nesekura Konekto"),
        ("Scale original", "Skalo originalo"),
        ("Scale adaptive", "Skalo adapta"),
        ("General", ""),
        ("Security", ""),
        ("Theme", ""),
        ("Dark Theme", ""),
        ("Light Theme", ""),
        ("Dark", ""),
        ("Light", ""),
        ("Follow System", ""),
        ("Enable hardware codec", ""),
        ("Unlock Security Settings", ""),
        ("Enable audio", ""),
        ("Unlock Network Settings", ""),
        ("Server", ""),
        ("Direct IP Access", ""),
        ("Proxy", ""),
        ("Apply", ""),
        ("Disconnect all devices?", ""),
        ("Clear", ""),
        ("Audio Input Device", ""),
        ("Use IP Whitelisting", ""),
        ("Network", ""),
        ("Pin Toolbar", ""),
        ("Unpin Toolbar", ""),
        ("Recording", ""),
        ("Directory", ""),
        ("Automatically record incoming sessions", ""),
        ("Change", ""),
        ("Start session recording", ""),
        ("Stop session recording", ""),
        ("Enable recording session", ""),
        ("Enable LAN discovery", ""),
        ("Deny LAN discovery", ""),
        ("Write a message", ""),
        ("Prompt", ""),
        ("Please wait for confirmation of UAC...", ""),
        ("elevated_foreground_window_tip", ""),
        ("Disconnected", ""),
        ("Other", ""),
        ("Confirm before closing multiple tabs", ""),
        ("Keyboard Settings", ""),
        ("Full Access", ""),
        ("Screen Share", ""),
        ("Wayland requires Ubuntu 21.04 or higher version.", "Wayland postulas Ubuntu 21.04 aŭ pli altan version."),
        ("Wayland requires higher version of linux distro. Please try X11 desktop or change your OS.", "Wayland postulas pli altan version de linuksa distro. Bonvolu provi X11-labortablon aŭ ŝanĝi vian OS."),
        ("JumpLink", "View"),
        ("Please Select the screen to be shared(Operate on the peer side).", "Bonvolu Elekti la ekranon por esti dividita (Funkciu ĉe la sama flanko)."),
        ("Show RustDesk", ""),
        ("This PC", ""),
        ("or", ""),
        ("Continue with", ""),
        ("Elevate", ""),
        ("Zoom cursor", ""),
        ("Accept sessions via password", ""),
        ("Accept sessions via click", ""),
        ("Accept sessions via both", ""),
        ("Please wait for the remote side to accept your session request...", ""),
        ("One-time Password", ""),
        ("Use one-time password", ""),
        ("One-time password length", ""),
        ("Request access to your device", ""),
        ("Hide connection management window", ""),
        ("hide_cm_tip", ""),
        ("wayland_experiment_tip", ""),
        ("Right click to select tabs", ""),
        ("Skipped", ""),
        ("Add to address book", ""),
        ("Group", ""),
        ("Search", ""),
        ("Closed manually by web console", ""),
        ("Local keyboard type", ""),
        ("Select local keyboard type", ""),
        ("software_render_tip", ""),
        ("Always use software rendering", ""),
        ("config_input", ""),
        ("config_microphone", ""),
        ("request_elevation_tip", ""),
        ("Wait", ""),
        ("Elevation Error", ""),
        ("Ask the remote user for authentication", ""),
        ("Choose this if the remote account is administrator", ""),
        ("Transmit the username and password of administrator", ""),
        ("still_click_uac_tip", ""),
        ("Request Elevation", ""),
        ("wait_accept_uac_tip", ""),
        ("Elevate successfully", ""),
        ("uppercase", ""),
        ("lowercase", ""),
        ("digit", ""),
        ("special character", ""),
        ("length>=8", ""),
        ("Weak", ""),
        ("Medium", ""),
        ("Strong", ""),
        ("Switch Sides", ""),
        ("Please confirm if you want to share your desktop?", ""),
        ("Display", ""),
        ("Default View Style", ""),
        ("Default Scroll Style", ""),
        ("Default Image Quality", ""),
        ("Default Codec", ""),
        ("Bitrate", ""),
        ("FPS", ""),
        ("Auto", ""),
        ("Other Default Options", ""),
        ("Voice call", ""),
        ("Text chat", ""),
        ("Stop voice call", ""),
        ("relay_hint_tip", ""),
        ("Reconnect", ""),
        ("Codec", ""),
        ("Resolution", ""),
        ("No transfers in progress", ""),
        ("Set one-time password length", ""),
        ("RDP Settings", ""),
        ("Sort by", ""),
        ("New Connection", ""),
        ("Restore", ""),
        ("Minimize", ""),
        ("Maximize", ""),
        ("Your Device", ""),
        ("empty_recent_tip", ""),
        ("empty_favorite_tip", ""),
        ("empty_lan_tip", ""),
        ("empty_address_book_tip", ""),
        ("eg: admin", ""),
        ("Empty Username", ""),
        ("Empty Password", ""),
        ("Me", ""),
        ("identical_file_tip", ""),
        ("show_monitors_tip", ""),
        ("View Mode", ""),
        ("login_linux_tip", ""),
        ("verify_rustdesk_password_tip", ""),
        ("remember_account_tip", ""),
        ("os_account_desk_tip", ""),
        ("OS Account", ""),
        ("another_user_login_title_tip", ""),
        ("another_user_login_text_tip", ""),
        ("xorg_not_found_title_tip", ""),
        ("xorg_not_found_text_tip", ""),
        ("no_desktop_title_tip", ""),
        ("no_desktop_text_tip", ""),
        ("No need to elevate", ""),
        ("System Sound", ""),
        ("Default", ""),
        ("New RDP", ""),
        ("Fingerprint", ""),
        ("Copy Fingerprint", ""),
        ("no fingerprints", ""),
        ("Select a peer", ""),
        ("Select peers", ""),
        ("Plugins", ""),
        ("Uninstall", ""),
        ("Update", ""),
        ("Enable", ""),
        ("Disable", ""),
        ("Options", ""),
        ("resolution_original_tip", ""),
        ("resolution_fit_local_tip", ""),
        ("resolution_custom_tip", ""),
        ("Collapse toolbar", ""),
        ("Accept and Elevate", ""),
        ("accept_and_elevate_btn_tooltip", ""),
        ("clipboard_wait_response_timeout_tip", ""),
        ("Incoming connection", ""),
        ("Outgoing connection", ""),
        ("Exit", ""),
        ("Open", ""),
        ("logout_tip", ""),
        ("Service", ""),
        ("Start", ""),
        ("Stop", ""),
        ("exceed_max_devices", ""),
        ("Sync with recent sessions", ""),
        ("Sort tags", ""),
        ("Open connection in new tab", ""),
        ("Move tab to new window", ""),
        ("Can not be empty", ""),
        ("Already exists", ""),
        ("Change Password", ""),
        ("Refresh Password", ""),
        ("ID", ""),
        ("Grid View", ""),
        ("List View", ""),
        ("Select", ""),
        ("Toggle Tags", ""),
        ("pull_ab_failed_tip", ""),
        ("push_ab_failed_tip", ""),
        ("synced_peer_readded_tip", ""),
        ("Change Color", ""),
        ("Primary Color", ""),
        ("HSV Color", ""),
        ("Installation Successful!", ""),
        ("Installation failed!", ""),
        ("Reverse mouse wheel", ""),
        ("{} sessions", ""),
        ("scam_title", ""),
        ("scam_text1", ""),
        ("scam_text2", ""),
        ("Don't show again", ""),
        ("I Agree", ""),
        ("Decline", ""),
        ("Timeout in minutes", ""),
        ("auto_disconnect_option_tip", ""),
        ("Connection failed due to inactivity", ""),
        ("Check for software update on startup", ""),
        ("upgrade_rustdesk_server_pro_to_{}_tip", ""),
        ("pull_group_failed_tip", ""),
        ("Filter by intersection", ""),
        ("Remove wallpaper during incoming sessions", ""),
        ("Test", ""),
        ("display_is_plugged_out_msg", ""),
        ("No displays", ""),
        ("Open in new window", ""),
        ("Show displays as individual windows", ""),
        ("Use all my displays for the remote session", ""),
        ("selinux_tip", ""),
        ("Change view", ""),
        ("Big tiles", ""),
        ("Small tiles", ""),
        ("List", ""),
        ("Virtual display", ""),
        ("Plug out all", ""),
        ("True color (4:4:4)", ""),
        ("Enable blocking user input", ""),
        ("id_input_tip", ""),
        ("privacy_mode_impl_mag_tip", ""),
        ("privacy_mode_impl_virtual_display_tip", ""),
        ("Enter privacy mode", ""),
        ("Exit privacy mode", ""),
        ("idd_not_support_under_win10_2004_tip", ""),
        ("input_source_1_tip", ""),
        ("input_source_2_tip", ""),
        ("Swap control-command key", ""),
        ("swap-left-right-mouse", ""),
        ("2FA code", ""),
        ("More", ""),
        ("enable-2fa-title", ""),
        ("enable-2fa-desc", ""),
        ("wrong-2fa-code", ""),
        ("enter-2fa-title", ""),
        ("Email verification code must be 6 characters.", ""),
        ("2FA code must be 6 digits.", ""),
        ("Multiple Windows sessions found", ""),
        ("Please select the session you want to connect to", ""),
        ("powered_by_me", ""),
        ("outgoing_only_desk_tip", ""),
        ("preset_password_warning", ""),
        ("Security Alert", ""),
        ("My address book", ""),
        ("Personal", ""),
        ("Owner", ""),
        ("Set shared password", ""),
        ("Exist in", ""),
        ("Read-only", ""),
        ("Read/Write", ""),
        ("Full Control", ""),
        ("share_warning_tip", ""),
        ("Everyone", ""),
        ("ab_web_console_tip", ""),
        ("allow-only-conn-window-open-tip", ""),
        ("no_need_privacy_mode_no_physical_displays_tip", ""),
        ("Follow remote cursor", ""),
        ("Follow remote window focus", ""),
        ("default_proxy_tip", ""),
        ("no_audio_input_device_tip", ""),
        ("Incoming", ""),
        ("Outgoing", ""),
        ("Clear Wayland screen selection", ""),
        ("clear_Wayland_screen_selection_tip", ""),
        ("confirm_clear_Wayland_screen_selection_tip", ""),
        ("android_new_voice_call_tip", ""),
        ("texture_render_tip", ""),
        ("Use texture rendering", ""),
        ("Floating window", ""),
        ("floating_window_tip", ""),
        ("Keep screen on", ""),
        ("Never", ""),
        ("During controlled", ""),
        ("During service is on", ""),
        ("Capture screen using DirectX", ""),
        ("Back", ""),
        ("Apps", ""),
        ("Volume up", ""),
        ("Volume down", ""),
        ("Power", ""),
        ("Telegram bot", ""),
        ("enable-bot-tip", ""),
        ("enable-bot-desc", ""),
        ("cancel-2fa-confirm-tip", ""),
        ("cancel-bot-confirm-tip", ""),
        ("About RustDesk", ""),
        ("Send clipboard keystrokes", ""),
        ("network_error_tip", ""),
        ("Unlock with PIN", ""),
        ("Requires at least {} characters", ""),
        ("Wrong PIN", ""),
        ("Set PIN", ""),
        ("Enable trusted devices", ""),
        ("Manage trusted devices", ""),
        ("Platform", ""),
        ("Days remaining", ""),
        ("enable-trusted-devices-tip", ""),
        ("Parent directory", ""),
        ("Resume", ""),
        ("Invalid file name", ""),
        ("one-way-file-transfer-tip", ""),
        ("Authentication Required", ""),
        ("Authenticate", ""),
        ("web_id_input_tip", ""),
        ("Download", ""),
        ("Upload folder", ""),
        ("Upload files", ""),
        ("Clipboard is synchronized", ""),
        ("Soft keyboard", ""),
    ].iter().cloned().collect();
}
