
# Projeto Virada VI
<p align="center">
  <img src="./banner.gif" width="100%" alt="Projeto Virada VI">
</p>
![Plataforma](https://img.shields.io/badge/Plataforma-Windows%2011-blue)
![GPU](https://img.shields.io/badge/GPU-GTX%201650-76B900)
![Status](https://img.shields.io/badge/Status-Experimental-orange)
![Licença](https://img.shields.io/badge/Licença-GPL--3.0-blue)

## Sobre o projeto

O Projeto Virada VI é um projeto experimental baseado no VIctory, com foco em compatibilidade, otimização e testes de desempenho em computadores equipados com placas de vídeo de entrada.

## Teste na GTX 1650

| Componente | Configuração |
|---|---|
| GPU | NVIDIA GeForce GTX 1650 |
| VRAM | 4 GB |
| Memória RAM | 16 GB |
| Sistema operacional | Windows 11 x64 |
| Perfil | Xbox Series X |
| Resolução interna | 1080p |
| Resolução de saída | 1440p |
| Ray Tracing | Desativado |

### Resultados de desempenho

O desbloqueio do limite de FPS do runtime permitiu ultrapassar os 30 FPS em cenas leves.

Em 14 amostras com pelo menos 1.000 comandos de desenho por quadro, a mediana registrada foi de 0,8 FPS.

O ganho de desempenho durante a missão ainda precisa ser medido por meio de uma comparação na mesma cena.

> [!NOTE]
> O projeto está em fase experimental. O desempenho e a estabilidade podem variar conforme o hardware e a configuração utilizada.

## Instalação

### 1. Pré-requisitos

- [Rust para Windows](https://rustup.rs/)
- [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
- Desenvolvimento para desktop com C++
- SDK do Windows

### 2. Download

Baixe o código-fonte através da opção **Code → Download ZIP** e extraia a pasta.

### 3. Configuração

Abra o PowerShell na pasta que contém o arquivo `Cargo.toml` e execute:

```powershell
.\Setup.cmd -InstallDir "$env:LOCALAPPDATA\VIctory-GTX1650"
```

Quando o seletor for aberto, indique a pasta de instalação do jogo.

### 4. Execução

Para utilizar o perfil testado com o limite de FPS desativado, execute:

```powershell
$env:VICTORY_UNCAP_FPS = '1'
$env:VICTORY_FSR_4K = '0'
$env:VICTORY_AMBIENT_FALLBACK = '1'

& "$env:LOCALAPPDATA\VIctory-GTX1650\victory-play.exe" --no-rt --console series-x --1080p-to-1440p --sparse-textures
```

## Créditos

O Projeto Virada VI utiliza como base os seguintes projetos:

- [DexNom RayTime](https://github.com/DexNom/VIctory)
- [VIctory](https://github.com/Marvisak/VIctory)

Consulte também os [créditos e a documentação original](https://github.com/donnie-ops/Projeto-viradaVI/blob/main/README.upstream.md).

## Licença

O código do projeto segue a licença [GPL-3.0-or-later](https://github.com/donnie-ops/Projeto-viradaVI/blob/main/LICENSE).

---

**Projeto Virada VI — Desenvolvimento experimental.**
