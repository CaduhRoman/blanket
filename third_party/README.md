# third_party

Cópias de crates do Servo com correções que a API de embedder não permite aplicar de fora.
Cada alteração está marcada no código com `[Blanket patch]`.

| Crate | Versão base | Correção |
| --- | --- | --- |
| `servo-net` | 0.8.0 | `connector.rs`: `http2_max_header_list_size` de 16 KiB → 256 KiB. Sem isto, YouTube e outros sites Google falham com `client error (SendRequest)` (h2 `PROTOCOL_ERROR`, cabeçalhos de resposta acima do limite). |

Ao atualizar o Servo, verificar se a correção já existe a montante e remover o patch.
