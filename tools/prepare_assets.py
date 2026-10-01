"""Prepare a local extracted Skate 3 disc without a scene editor."""
from pathlib import Path
import argparse,sys
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from tools.asset_pipeline.versions import installed


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--game-root',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--game-exe',type=Path,required=True)
    parser.add_argument('--character-only',action='store_true',
        help='Prepare only the character customiser into the existing installation, leaving maps untouched')
    args=parser.parse_args()
    report=lambda text:print(text,flush=True)
    from tools.asset_pipeline.customiser_setup import install,prepare
    from tools.asset_pipeline.setup_state import source_directory
    previous=installed(args.output)
    if args.character_only:
        if previous is None:
            raise SystemExit(f'No installation in {args.output}. Run without --character-only first.')
        prepare(source_directory(args.game_root),previous[0]/'assets',report)
        return
    install(args.game_root,args.output,args.game_exe.resolve(),report,refresh=previous is not None)

if __name__=='__main__':main()
