public class m7
{
	private int e;

	private List<MyPositionData> g;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			e = value;
		}
	}

	public byte MyPositionSelect
	{
		get { return 0; }
	}

	public bool BuiltInGps
	{
		get { return false; }
	}

	public int Interval
	{
		get { return 0; }
	}

	public List<MyPositionData> MyPositionList
	{
		get
		{
			return g;
		}
	}

	public void ai()
	{
		int num3 = 0;
		while (num3 < 5)
		{
			g.Add(new MyPositionData(null));
			g[num3].OffsetProgrammableMemoryAddress = e;
			num3++;
		}
	}

	public void a6(n7 A_0)
	{
		int num3 = 0;
		A_0.a(MyPositionSelect, 329392 + e);
		A_0.b(Interval, 2, 329222 + e);
		A_0.a(BuiltInGps, 329216 + e);
		while (num3 < 5)
		{
			g[num3].a3(A_0, num3);
			num3++;
		}
	}

	public void a7(n7 A_0)
	{
		MyPositionSelect = A_0.a(329392 + e);
	}
}
