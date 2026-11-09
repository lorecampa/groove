# Revisione del piano Groove: punti critici e migliorie

Destinatario: agente incaricato di aggiornare `docs/architecture.md` e
`docs/implementation-plan.md` (e, se serve, `README.md`).

## Vincoli da rispettare

- **Non introdurre `run` nella v1.** Resta tra le funzionalità rimandate, come
  già scritto nei documenti. La superficie CLI della v1 rimane `init`,
  `acquire`, `release`.
- Mantieni gli invarianti esistenti: ownership tramite `flock` sul descrittore,
  nessun daemon, file di lock permanenti, verifica prima di ogni consegna,
  worktree in HEAD detached.
- Mantieni la tabella dei crash del milestone 8 e la regola "niente test basati
  su `sleep`": sono punti di forza.
- Cambia solo la documentazione: non scrivere codice.

## Punti critici

### 1. Chi usa la CLI `acquire` (critico)

`acquire` consegna il lock tramite un socket `SOCK_SEQPACKET` ereditato e
`SCM_RIGHTS`. Da shell (bash, step di CI, agenti che eseguono comandi) questo
non si può fare: serve un programma che crei la socketpair, passi il fd e
riceva il descrittore. Il README però promette "agent-first", e oggi questa
promessa non è coerente con l'interfaccia.

Cosa fare nei documenti:

- Dichiara in modo esplicito chi usa la CLI nella v1: processi supervisori o
  programmi in grado di fare fd passing, non script di shell.
- Aggiungi al milestone 6 un **client di riferimento minimale** (per esempio un
  piccolo binario o esempio Rust, `groove-cli/examples/client.rs`, oppure uno
  snippet Python con `socket.recv_fds`). Deve mostrare come creare il canale,
  ricevere il descrittore, tenerlo per tutta la durata del task e chiamare
  `release`. Senza questo client il protocollo non è verificabile da un utente
  esterno.
- Documenta in `docs/protocol.md` un esempio completo di invocazione dal punto
  di vista del chiamante.
- Aggiorna la sezione "Design goals" del README in modo che non prometta l'uso
  diretto da shell.

### 2. Processi in background che ereditano il descrittore (critico)

Il modello prevede che i writer ereditino il fd del lease. Nella pratica build
e agenti avviano processi che vivono a lungo: server di sccache o Gradle, file
watcher, dev server, language server. Se ereditano il fd, lo slot resta
occupato a tempo indeterminato e nulla lo segnala.

Cosa fare nei documenti:

- Aggiungi questo rischio in modo esplicito all'architettura (sezione "Locking
  and ownership") e alla documentazione operativa (`docs/operations.md`).
- Raccomanda ai chiamanti di passare il fd solo ai writer diretti, di lanciarli
  in un process group dedicato e di terminare il gruppo prima di `release`.
- Aggiungi alla tabella dei crash: "Un processo daemon nipote eredita il
  descrittore → lo slot resta occupato; la diagnostica lo rende visibile".
- Valuta una diagnostica di sola lettura che, dato uno slot occupato, elenchi i
  processi che tengono aperto il file di lock (scansione di `/proc/*/fd`).
  Rendila disponibile tramite i dettagli di `POOL_EXHAUSTED`, oppure segnala che
  arriverà con `status`. Serve solo a diagnosticare: non deve mai decidere se
  uno slot è disponibile.

### 3. `git clean -ffdx` annulla il vantaggio del pool (importante)

Se a ogni acquisizione si cancellano anche i file ignorati (`target/`,
`node_modules/`, `.venv`), riusare un worktree conviene poco rispetto a
crearne uno nuovo. Per agenti e CI il vantaggio principale di un pool è avere
le cache di build già calde.

Cosa fare nei documenti:

- Introduci una politica di pulizia configurabile a livello di pool (in
  `config.json`, scelta durante `init`):
  - `pristine`: `clean -ffdx`, il comportamento attuale;
  - `warm`: `clean -ffd`, che rimuove i file non tracciati ma conserva quelli
    ignorati.
- Scegli e motiva il default (consiglio: `pristine` per sicurezza, `warm` come
  opzione esplicita).
- Adatta la verifica: in modalità `warm` l'assenza di file ignorati non va
  verificata.
- Aggiorna lo schema `config-v1`, i criteri di accettazione del milestone 4 e
  la tabella dei crash ("Staged edits and ignored artifacts").

### 4. Sequenza di normalizzazione ridondante (minore)

`git restore --source=<OID> --staged --worktree -- .` è ridondante, perché
`git checkout --detach --force <OID>` riallinea già indice e file tracciati.

Cosa fare: riduci la sequenza a `checkout --detach --force <OID>` più il
`clean` della politica scelta, seguiti dalla verifica. Se vuoi tenere `restore`,
spiega quale caso copre che `checkout --force` non copre.

### 5. Worktree dentro `.git/groove/worktrees/` (da verificare)

Tenere tutto dentro il repository è comodo, ma comporta dei rischi:

- i tool che copiano o fanno backup di `.git` si portano dietro interi
  checkout;
- alcuni tool trattano in modo speciale i percorsi che contengono `.git/`
  (file watcher, editor, linter, regole di ignore).

Cosa fare:

- Aggiungi a `init` un'opzione per la root dei worktree. Il default resta
  `<GIT_COMMON_DIR>/groove/worktrees/`, con un'alternativa esplicita fuori da
  `.git`. La validazione dei percorsi gestiti va estesa di conseguenza.
- Oppure, come minimo, aggiungi al milestone 4 un test di compatibilità: build
  cargo e npm eseguite dentro uno slot, e `git status` o `git log` lanciati
  dall'interno dello slot.

### 6. Ordine dei milestone (importante)

Il milestone 1 chiede subito JSON Schema, help in JSON, `NOT_IMPLEMENTED`
strutturati e CI con versioni minime. È rifinitura che arriva prima di aver
dimostrato che il cuore del sistema funziona.

Cosa fare: riordina il piano partendo da una versione minima end-to-end.

1. **Prototipo minimo:** workspace, discovery, `init`, `SlotGuard` con `flock`,
   creazione del worktree detached, normalizzazione e verifica di base, `Pool`
   e `Lease` nella libreria, e un test di concorrenza con N processi su M slot.
   Ancora niente CLI, niente schemi, niente socket.
2. Recovery e quarantena (l'attuale milestone 5, che è la parte con più valore).
3. Protocollo JSON, schemi, codici di uscita, help e errori in JSON.
4. CLI `acquire` e `release` con `SCM_RIGHTS` e il client di riferimento.
5. Esempio di integrazione Rust.
6. Gate end-to-end.

Nel prototipo aggiungi un confronto esplicito con uno script bash di base
(`flock` + `git worktree`), per mostrare cosa aggiunge Groove: verifica,
recovery, quarantena, limite di capacità.

### 7. Semplificare `release` (da valutare)

Visto che la pulizia avviene alla successiva acquisizione, chiudere l'ultimo
descrittore basta già a liberare lo slot. `release` esplicito aggiunge solo i
metadati di rilascio, ma porta con sé un caso difficile: la risposta persa e il
retry che non deve toccare un lease più recente.

Cosa fare: mantieni `release` nella v1, ma documenta che è facoltativo e che
chiudere il descrittore è un rilascio valido. Valuta se i metadati di rilascio
servono davvero per la v1: se servono solo a scopo diagnostico, dillo
esplicitamente.

## Migliorie minori

- **`--request-id` facoltativo:** se manca, la risposta riporta
  `request_id: null` (lo schema delle risposte lo prevede già per gli errori).
- **Branch e ref creati dagli agenti:** dichiara che restano nel repository
  condiviso dopo il rilascio e che Groove non li gestisce. Aggiungi un test:
  la sanitizzazione non cancella i branch creati da un lease precedente.
- **Filesystem non locali:** durante `init` e `acquire`, rileva il tipo di
  filesystem della root Groove (`statfs`). Rifiuta NFS, CIFS e FUSE con
  `UNSUPPORTED_CONFIGURATION`, invece di dare per scontato che sia locale.
- **`git worktree prune`:** valuta se registrare i worktree con
  `git worktree lock`, per evitare che un `prune` esterno rimuova la
  registrazione di uno slot. Se non lo fai, documenta il motivo.
- **Versione minima del kernel:** se usi API recenti (per esempio
  `MSG_CMSG_CLOEXEC`, o in futuro `pidfd`), dichiara la versione minima del
  kernel Linux nella sezione dei requisiti.

## Risultato atteso

- `docs/architecture.md` e `docs/implementation-plan.md` aggiornati secondo i
  punti 1–7 e le migliorie minori, coerenti tra loro.
- `run` ancora tra le funzionalità rimandate.
- README allineato: obiettivi, elenco dei milestone riordinato, utenti target
  della CLI.
- Ogni scelta fatta su un punto "da valutare" o "da verificare" motivata in una
  riga nel documento.
