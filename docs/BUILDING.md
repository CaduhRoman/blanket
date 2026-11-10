# Compilar o Blanket

## Windows 10/11 x64

1. Instale o [Rust](https://rustup.rs/) (MSVC). O workspace pede `rust-version = "1.88"`.
2. Instale o Visual Studio Build Tools com o workload C++.
3. Instale LLVM e coloque `libclang.dll` no `PATH` (o `mozangle` do Servo precisa de `bindgen`).
4. Python 3 no `PATH`.

O perfil `dev` otimiza as dependências (`opt-level = 2`) e o código do Blanket em `opt-level = 1`. Sem isso o Servo fica inutilizável. LTO completo no release precisa de muito mais de 8 GB de RAM no linker; por isso `lto = false` é o padrão.

```powershell
rustc -V
cargo test -p blanket-core -p blanket-net
cargo build -p blanket --release
```

O binário fica em `target/release/blanket.exe`. Builds de debug do crate `blanket` também funcionam, mas o arranque é mais lento.

## Funcionalidades opcionais

| Feature | Efeito | Dependência extra |
| --- | --- | --- |
| `webgl` (padrão) | WebGL via Servo | nenhuma |
| `media` | áudio/vídeo com GStreamer | runtime e SDK GStreamer MSVC |
| `devtools` | servidor de developer tools do Servo | nenhuma |

```powershell
cargo run -p blanket --release --features media
```

## Linux e macOS

A arquitetura (winit + egui + Servo) é portável. O alvo inicial é só Windows: o ícone, o diálogo nativo de ficheiros e `explorer.exe` para revelar downloads estão escritos com isso em mente. Portar significa:

- deixar de assumir fontes Segoe / Yu Gothic / Malgun no chrome;
- trocar `reveal_in_file_manager` pelo opener da plataforma (já há um ramo não-Windows);
- testar o contexto GL/ANGLE do Servo nessa OS.

## Variáveis úteis

| Variável | Função |
| --- | --- |
| `BLANKET_PROFILE_DIR` | Perfil (o `blanket-bench` usa isto para runs limpos) |
| `BLANKET_LOG` | Filtro `tracing` (`EnvFilter`). Padrão: `warn,blanket=info,...` |
