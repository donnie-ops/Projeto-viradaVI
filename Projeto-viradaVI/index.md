# Virada

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

O projeto é distribuído como código-fonte sob a licença GPL-3.0-or-later. A compilação usa Rust e Visual Studio Build Tools no Windows. Siga as [instruções de instalação no README](https://github.com/donnie-ops/Projeto-viradaVI#instalação) e use sua própria instalação do jogo.
