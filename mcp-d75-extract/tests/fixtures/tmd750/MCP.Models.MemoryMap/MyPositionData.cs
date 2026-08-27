public class MyPositionData
{
	private int av;

	protected int aw;

	protected byte ax;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			av = value;
		}
	}

	public int Altitude
	{
		get { return aw; }
	}

	public byte MyPositionChannel
	{
		get { return ax; }
	}

	public string Name
	{
		get { return e; }
	}

	public override void a3(n7 A_0, int A_1)
	{
		int num = 329232 + av + 32 * A_1;
		A_0.a(c, num + 12);
		A_0.b(aw, 4, num);
		A_0.a(g, 2, num + 12);
		A_0.a(j, num + 4);
		A_0.a(m, num + 5);
		A_0.b(p, 2, num + 6);
		A_0.a(s, 3, num + 12);
		A_0.a(v, num + 8);
		A_0.a(y, num + 9);
		A_0.b(ab, 2, num + 10);
		A_0.a(ax, num + 13);
		A_0.c(e, num + 14, oc.y);
	}

	public override void a4(n7 A_0, int A_1)
	{
		int num = 329232 + av + 32 * A_1;
		aw = A_0.h(num, 4);
	}
}
