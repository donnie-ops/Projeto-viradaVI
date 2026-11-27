# ViradaVI

Runtime experimental de compatibilidade com Xbox Series para Windows x64. O Virada traduz comandos gráficos do console para Direct3D 12 e é baseado no DexNom RayTime e no VIctory.

[Ver código-fonte no GitHub](https://github.com/donnie-ops/Projeto-viradaVI)

## Teste na GTX 1650

| Componente | Configuração |
| --- | --- |
| GPU | NVIDIA GeForce GTX 1650 com 4 GB de VRAM |
| RAM | 16 GB |
| Sistema | Windows 11 x64 |
| Perfil | Series X, cena em 1080p, saída em 1440p e ray tracing desligado |

O desbloqueio do limite do runtime passou dos 30 FPS em cenas leves. Em 14 amostras com pelo menos 1.000 comandos de desenho por quadro, a mediana foi de 0,8 FPS. O ganho de desempenho durante a missão ainda precisa ser medido em uma comparação na mesma cena.

## Instalação

1. Instale o [Rust para Windows](https://rustup.rs/) e o [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) com **Desenvolvimento para desktop com C++** e o SDK do Windows.
2. Baixe o código em **Code → Download ZIP** e extraia a pasta.
3. Abra um PowerShell na pasta que contém `Cargo.toml` e rode o instalador. Quando abrir o seletor, indique o executável da sua instalação do jogo.

```powershell
.\Setup.cmd -InstallDir "$env:LOCALAPPDATA\VIctory-GTX1650"
```

Para usar o perfil testado com o limite do runtime desativado:

```powershell
$env:VICTORY_UNCAP_FPS = '1'
$env:VICTORY_FSR_4K = '0'
$env:VICTORY_AMBIENT_FALLBACK = '1'
& "$env:LOCALAPPDATA\VIctory-GTX1650\victory-play.exe" --no-rt --console series-x --1080p-to-1440p --sparse-textures
```

O código do projeto segue a [GPL-3.0-or-later](LICENSE). Os projetos [DexNom RayTime](https://github.com/DexNom/VIctory) e [VIctory](https://github.com/Marvisak/VIctory) são a base do Virada. Veja também os [créditos e a documentação original](README.upstream.md).
