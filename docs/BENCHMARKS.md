# Benchmarks

O crate `blanket-bench` lança os browsers com **perfis temporários**, as **mesmas URLs** e as **mesmas contagens de abas**, e grava JSON.

Não compare um número de marketing com outro. Corra o harness na sua máquina e leia o ficheiro.

## O que é medido

| Campo | Significado |
| --- | --- |
| `startup_ms` | Tempo até o processo estar de pé *mais* o período de settle. É um teto, não o first paint. |
| `first_frame_ms` | Só Blanket: evento `first_frame` do `--metrics-file` (primeira moldura apresentada). |
| `load_complete_ms` | Só Blanket: `load_ms` do evento `load_complete` da aba. |
| `rss_bytes` | Working set do processo após o settle. |
| `idle_cpu_percent` | `sysinfo` após mais `idle_secs` parado. Percentagem de *um* núcleo. |

Chrome e Firefox não expõem first-frame pelo mesmo canal; esses campos ficam `null` para eles. O RSS e o CPU *são* comparáveis, desde que a página e o número de abas coincidam.

## Como correr

```powershell
cargo build -p blanket --release
cargo run -p blanket-bench --release -- `
  --url https://example.com/ `
  --tabs 1,5,10,20 `
  --settle-secs 12 `
  --idle-secs 5 `
  --output bench-results/example.json
```

Browsers em falta são saltados. 50 abas é um teste pesado: use `--tabs 50` só quando quiser esse ponto, e feche outros programas pesados.

## Condições equivalentes

- Perfil novo em diretório temporário (sem extensões, sem sessão anterior).
- Blanket: `--no-restore` e `BLANKET` isolado via `--profile-dir`.
- Chrome: `--user-data-dir`, `--no-first-run`, `--disable-sync`, `--disable-extensions`.
- Firefox: `-profile` temporário, `-no-remote`.

Se um browser restaurar um “what's new” ou um wizard, o RSS deixa de ser comparável — o harness tenta desligar isso, mas UIs de primeira execução mudam com as versões.

## O que o harness não mede (ainda)

- Latência de input (clique → paint).
- Tempo de acordar uma aba suspensa com 50 abas abertas — o Blanket grava `tab_restore_first_frame` no metrics file; o harness ainda não agrega esse evento em série. Pode ler o JSONL do perfil.
- Sites que o Servo ainda não pinta corretamente: medir “load complete” neles não é o mesmo que no Chrome.
