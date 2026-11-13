import sys
from pathlib import Path
sys.path.insert(0, str(Path.cwd() / 'tests/bench'))
import macos_workflows as m
m.BCTL = Path(sys.argv.pop(1)).resolve()
with m.interruptions():
    sys.exit(m.main())
