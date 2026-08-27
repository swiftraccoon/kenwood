public class ObjectData : BasePositionData
{
	public enum a : byte
	{
		a,
		b,
	}

	public enum b : byte
	{
		a,
		b,
		c,
	}

	private int at;

	private a av;

	private b ax;

	private byte az;

	private new byte m_a3;

	private string a5;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			at = value;
		}
	}

	public a ObjectTxFormat
	{
		get { return av; }
	}

	public b ObjectTxInterval
	{
		get { return ax; }
	}

	public byte ObjectTable
	{
		get { return az; }
	}

	public byte ObjectSymbol
	{
		get { return this.m_a3; }
	}

	public string ObjectComment
	{
		get { return a5; }
	}

	public override void a3(n7 A_0, int A_1)
	{
		int num = 331264 + at + 64 * A_1;
		A_0.a((byte)0, num + 8);
		A_0.a(g, 2, num + 8);
		A_0.a(j, num);
		A_0.a(m, num + 1);
		A_0.b(p, 2, num + 2);
		A_0.a(s, 3, num + 8);
		A_0.a(v, num + 4);
		A_0.a(y, num + 5);
		A_0.b(ab, 2, num + 6);
		A_0.c(e, num + 9, oc.aq);
		A_0.a((byte)ObjectTxFormat, num + 18);
		A_0.a((byte)ObjectTxInterval, num + 19);
		A_0.a(ObjectTable, num + 20);
		A_0.a(ObjectSymbol, num + 21);
		A_0.d(ObjectComment, num + 22, oc.ar);
	}

	public override void a4(n7 A_0, int A_1)
	{
		int num = 331264 + at + 64 * A_1;
		g = A_0.d(num + 8, 2);
		j = A_0.a(num);
		m = A_0.a(num + 1);
		p = A_0.h(num + 2, 2);
		s = A_0.d(num + 8, 3);
		v = A_0.a(num + 4);
		y = A_0.a(num + 5);
		ab = A_0.h(num + 6, 2);
		e = A_0.e(num + 9, oc.aq);
		av = (a)A_0.a(num + 18);
		ax = (b)A_0.a(num + 19);
		az = A_0.a(num + 20);
		this.m_a3 = A_0.a(num + 21);
		a5 = A_0.g(num + 22, oc.ar);
	}
}
