// #[cfg(target_os = "windows")]
// use std::process::Command;

// /// Funzione Entry Point: Cerca di configurare il firewall se siamo su Windows.
// /// Su Mac/Linux non fa nulla (ritorna subito).
// pub fn ensure_open_port() {
//     #[cfg(target_os = "windows")]
//     windows_implementation();
// }

// #[cfg(target_os = "windows")]
// fn windows_implementation() {
//     let rule_name = "RustClip TCP";
//     let port = 5566;

//     // 1. Controlliamo se la regola esiste già
//     // Comando: netsh advfirewall firewall show rule name="RustClip TCP"
//     let check = Command::new("netsh")
//         .args(["advfirewall", "firewall", "show", "rule", &format!("name={}", rule_name)])
//         .output();

//     // Se il comando ha successo (exit code 0), la regola esiste. Non facciamo nulla.
//     if let Ok(output) = check {
//         if output.status.success() {
//             // Regola trovata, siamo a posto.
//             return;
//         }
//     }

//     // 2. Se siamo qui, la regola manca. Proviamo ad aggiungerla.
//     println!("🛡️  Rilevato Windows: Tento di configurare il Firewall per la porta {}...", port);

//     // Comando: netsh advfirewall firewall add rule name="RustClip TCP" dir=in action=allow protocol=TCP localport=5566
//     let result = Command::new("netsh")
//         .args([
//             "advfirewall", "firewall", "add", "rule",
//             &format!("name={}", rule_name),
//             "dir=in",
//             "action=allow",
//             "protocol=TCP",
//             &format!("localport={}", port)
//         ])
//         .output();

//     match result {
//         Ok(output) if output.status.success() => {
//             println!("✅ Regola Firewall aggiunta con successo! Il traffico in entrata è permesso.");
//         },
//         _ => {
//             // 3. Fallimento (probabilmente mancano i permessi di Admin)
//             // Avvisiamo l'utente in modo chiaro.
//             eprintln!("\n⚠️  ATTENZIONE: Impossibile configurare automaticamente il Firewall.");
//             eprintln!("   Windows bloccherà le connessioni in entrata dal Mac.");
//             eprintln!("   PER RISOLVERE: Apri PowerShell come Amministratore e incolla questo comando:");
//             eprintln!("   --------------------------------------------------------------------------------");
//             eprintln!("   netsh advfirewall firewall add rule name=\"{}\" dir=in action=allow protocol=TCP localport={}", rule_name, port);
//             eprintln!("   --------------------------------------------------------------------------------\n");
//         }
//     }
// }